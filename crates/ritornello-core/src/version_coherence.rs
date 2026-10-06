//! Guard over the versioning scheme: every shipped component declares its own
//! patch version, and none of them drifts off the product's generation.
//!
//! Three numbers exist in this repository and only one of them is here. The
//! product number lives in `[workspace.package] version` and names the
//! release; each shipped component declares its own version so a fix in one
//! plugin does not renumber the whole product — which would make every
//! component look stale on the device and have the updater replace all of
//! them. `ritornello_proto::PROTOCOL_VERSION` is the compatibility contract
//! and is none of this file's business.
//!
//! What a red test here means: either a component started inheriting the
//! product number again (so it can no longer be fixed on its own), or one
//! drifted off the shared generation (so `0.2.x` no longer means one thing).

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    /// The ten plugins shipped as their own archive, plus the core. Listed
    /// here AND checked against `deploy/plugins.example.toml` below, so a
    /// plugin added to the repository without a version of its own is a red
    /// test rather than an archive named after the wrong number.
    const SHIPPED_PLUGINS: &[&str] = &[
        "radio",
        "cd",
        "musicbrainz",
        "nrj-metas",
        "ouifm-metas",
        "radiofrance-metas",
        "files",
        "console",
        "generic-input",
        "mpd",
    ];

    /// Components that ship beside a plugin and are versioned on their own,
    /// but are not plugins: no `ritornello-plugin-` prefix, no entry in
    /// `plugins.example.toml`. Each is a crate that declares its own literal
    /// version, and every rule below that walks the shipped components walks
    /// these too.
    const SHIPPED_COMPANIONS: &[&str] = &["ritornello-files-mount"];

    /// Crates that legitimately keep inheriting the product number: no
    /// archive is named after them. `ritornello-updater` is here because it
    /// travels inside the core's archive rather than as its own component.
    const INTERNAL_CRATES: &[&str] = &[
        "ritornello-proto",
        "ritornello-i18n",
        "ritornello-plugin-sdk",
        "ritornello-updater",
        // Shared plugins.toml editing: it inherits the product number and no
        // archive is named after it.
        "ritornello-manifest",
        // The workstation-side installer. Every release carries it, built
        // for five workstation targets by the `installer` job of ci.yml, and
        // it inherits the product number on purpose: no device ever fetches
        // it, so the equality rule that makes a component's own number
        // necessary never applies, and a person simply takes the one in the
        // newest release. Its archives are named after a workstation target
        // triple alone — no version, so that the README can link to
        // `releases/latest/download/<file>` — and never after a component.
        "ritornello-install",
    ];

    /// Every crate whose version names an archive: the core, the plugins and
    /// the companions. One list, so a rule cannot forget one of the three.
    fn shipped_crate_names() -> Vec<String> {
        let mut names = vec!["ritornello-core".to_string()];
        names.extend(
            SHIPPED_PLUGINS
                .iter()
                .map(|p| format!("ritornello-plugin-{p}")),
        );
        names.extend(SHIPPED_COMPANIONS.iter().map(|c| c.to_string()));
        names
    }

    /// Whether a shipped crate's number is tied to the product's.
    ///
    /// The core and the plugins are; a companion is NOT. A root-privileged
    /// companion (`files-mount`) has a number of its own, independent of the
    /// product's generation and of its prerelease suffix, and it moves only
    /// when the companion itself changes. The reason is the device's own
    /// rule: it compares a companion's version for equality to decide whether
    /// the in-app update of its plugin is allowed, and only
    /// `ritornello-install` can place the companion. Any move not caused by
    /// a real change of the companion therefore forces an installer run for
    /// nothing -- across a beta, and across a minor or major product change
    /// (0.2 -> 0.3 -> 1.0) just as much.
    fn tied_to_product(name: &str) -> bool {
        !SHIPPED_COMPANIONS.contains(&name)
    }

    /// The shipped crates whose number must follow the product rules
    /// (generation, finished number, prerelease suffix): everything but the
    /// companions.
    fn product_tied_crate_names() -> Vec<String> {
        shipped_crate_names()
            .into_iter()
            .filter(|n| tied_to_product(n))
            .collect()
    }

    /// Why `version` is off the product generation, or `None`. Companions are
    /// never off it: see `tied_to_product`.
    fn generation_problem(product: &str, name: &str, version: &str) -> Option<String> {
        if !tied_to_product(name) || generation(version) == generation(product) {
            return None;
        }
        Some(format!(
            "{name} is {version}, off the product generation of {product}; \
             only the third number is free"
        ))
    }

    /// `major.minor.patch` with an optional non-empty dot-separated
    /// prerelease: all a companion's own number has to be, since it is
    /// compared and named in an archive like any other.
    fn is_valid_semver(version: &str) -> bool {
        let (core, pre) = match version.split_once('-') {
            Some((c, p)) => (c, Some(p)),
            None => (version, None),
        };
        let parts: Vec<&str> = core.split('.').collect();
        let numbers = parts.len() == 3
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|c| c.is_ascii_digit()));
        let pre_ok = pre.is_none_or(|p| {
            p.split('.').all(|i| {
                !i.is_empty() && i.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-')
            })
        });
        numbers && pre_ok
    }

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn read(path: &Path) -> String {
        std::fs::read_to_string(path)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
    }

    /// The first `version = "..."` of a manifest, or `None` when the manifest
    /// inherits with `version.workspace = true`. Deliberately textual rather
    /// than a TOML parse: what this guard is about is which of the two forms
    /// is written, and a parse would erase that distinction.
    fn declared_version(manifest: &str) -> Option<String> {
        for line in manifest.lines() {
            let line = line.trim_end_matches('\r').trim();
            if let Some(rest) = line.strip_prefix("version = \"") {
                return rest.strip_suffix('"').map(str::to_string);
            }
            if line == "version.workspace = true" {
                return None;
            }
        }
        panic!("manifest declares no version at all");
    }

    /// The part after the first dash, if any: `beta.1` of `0.2.1-beta.1`.
    fn prerelease(version: &str) -> Option<&str> {
        version.split_once('-').map(|(_, suffix)| suffix)
    }

    /// The generation, `major.minor`, of a version that may carry a
    /// prerelease suffix.
    ///
    /// The suffix is cut off before counting components: a beta names the
    /// same generation as the delivery it prepares, so `0.2.1-beta.1` is
    /// `0.2` and not a version with four numbers in it.
    fn generation(version: &str) -> (String, String) {
        let core = version.split('-').next().unwrap_or(version);
        let mut parts = core.split('.');
        let major = parts.next().unwrap_or_default().to_string();
        let minor = parts.next().unwrap_or_default().to_string();
        let patch = parts.next().unwrap_or_default();
        assert!(
            !major.is_empty() && !minor.is_empty() && !patch.is_empty(),
            "version {version} is not major.minor.patch"
        );
        assert!(
            parts.next().is_none(),
            "version {version} has more than three components"
        );
        (major, minor)
    }

    fn product_version() -> String {
        let root = read(&repo_root().join("Cargo.toml"));
        declared_version(&root).expect("the workspace must declare a product version")
    }

    fn crate_manifest(name: &str) -> String {
        read(&repo_root().join("crates").join(name).join("Cargo.toml"))
    }

    /// The packs declared in `deploy/language-packs.toml`, as
    /// `(language, version)`. Their version has no Cargo.toml to live in --
    /// a pack is not a crate -- so this file is the one place it is written,
    /// and this guard is what keeps it on the same rails as every component.
    ///
    /// Deliberately textual, like `declared_version` above: a `[section]`
    /// header names the language and the `version = "..."` line that follows
    /// it names its number, and a full TOML parse is more machinery than two
    /// line shapes deserve.
    fn declared_packs() -> Vec<(String, String)> {
        let text = read(&repo_root().join("deploy").join("language-packs.toml"));
        let mut packs = Vec::new();
        let mut current: Option<String> = None;
        for line in text.lines() {
            let line = line.trim_end_matches('\r').trim();
            if let Some(lang) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                current = Some(lang.to_string());
                continue;
            }
            if let Some(rest) = line.strip_prefix("version = \"") {
                let version = rest
                    .strip_suffix('"')
                    .unwrap_or_else(|| panic!("malformed version line: {line}"));
                let lang = current
                    .clone()
                    .unwrap_or_else(|| panic!("version line before any [language] section"));
                packs.push((lang, version.to_string()));
            }
        }
        assert!(
            !packs.is_empty(),
            "deploy/language-packs.toml declares no language pack at all"
        );
        packs
    }

    /// The languages `deploy/locales` carries text for: every module
    /// directory it holds, deduplicated. A module without a given language's
    /// `.toml` is normal -- not every plugin needs its own strings -- so this
    /// asks only which languages exist anywhere under `deploy/locales`, not
    /// which modules a language completes.
    fn languages_on_disk() -> Vec<String> {
        let root = repo_root().join("deploy").join("locales");
        let mut languages = std::collections::BTreeSet::new();
        for module in std::fs::read_dir(&root)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", root.display()))
        {
            let module = module.expect("readable directory entry").path();
            if !module.is_dir() {
                continue;
            }
            for entry in std::fs::read_dir(&module)
                .unwrap_or_else(|e| panic!("cannot read {}: {e}", module.display()))
            {
                let entry = entry.expect("readable directory entry").path();
                if entry.extension().and_then(|e| e.to_str()) == Some("toml") {
                    let lang = entry
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or_else(|| panic!("non-UTF8 file name: {}", entry.display()));
                    languages.insert(lang.to_string());
                }
            }
        }
        assert!(
            !languages.is_empty(),
            "deploy/locales carries no language file at all"
        );
        languages.into_iter().collect()
    }

    #[test]
    fn every_shipped_component_declares_its_own_version() {
        let names = shipped_crate_names();
        for name in names {
            let declared = declared_version(&crate_manifest(&name));
            assert!(
                declared.is_some(),
                "{name} inherits the product version; it must declare its own \
                 so it can be fixed without renumbering everything"
            );
        }
    }

    #[test]
    fn every_shipped_component_stays_on_the_product_generation() {
        let product = product_version();
        for name in product_tied_crate_names() {
            let version = declared_version(&crate_manifest(&name))
                .unwrap_or_else(|| panic!("{name} declares no version of its own"));
            if let Some(why) = generation_problem(&product, &name, &version) {
                panic!("{why}");
            }
        }
    }

    /// A companion is exempt from every rule tying a component to the
    /// product (generation, prerelease suffix, finished number inside a
    /// prerelease, finished product refusing a suffix). Its number is its
    /// own and moves only when the companion itself changes, never with the
    /// product: the device compares it for equality to allow the in-app
    /// update of its plugin, and only `ritornello-install` can place it, so
    /// any move not caused by a real change forces an installer run for
    /// nothing.
    #[test]
    fn a_companion_need_not_share_the_products_generation() {
        let companion = SHIPPED_COMPANIONS[0];
        for product in ["0.2.0-beta.4", "1.3.0", "0.3.0-rc.1"] {
            assert_eq!(
                generation_problem(product, companion, "1.0.0"),
                None,
                "a companion at 1.0.0 inside {product}"
            );
            assert!(!tied_to_product(companion), "{product}");
        }
        assert!(
            generation_problem("0.3.0", "ritornello-core", "0.2.0").is_some(),
            "the core is still tied to the product generation"
        );
        assert!(
            generation_problem("0.3.0", "ritornello-plugin-radio", "0.2.9").is_some(),
            "a plugin is still tied to the product generation"
        );
        assert!(tied_to_product("ritornello-core"));
    }

    /// Every companion declares its own version, and it is valid semver.
    #[test]
    fn every_companion_declares_a_valid_semver_of_its_own() {
        for name in SHIPPED_COMPANIONS {
            let version = declared_version(&crate_manifest(name))
                .unwrap_or_else(|| panic!("{name} inherits the product version"));
            assert!(
                is_valid_semver(&version),
                "{name} declares {version}, which is not major.minor.patch[-prerelease]"
            );
        }
        for good in ["0.2.0", "1.0.0", "3.1.4", "0.2.0-beta.2"] {
            assert!(is_valid_semver(good), "{good}");
        }
        for bad in ["0.2", "0.2.0.1", "a.b.c", "0.2.0-", "0.2.0-beta..1", ""] {
            assert!(!is_valid_semver(bad), "{bad}");
        }
    }

    /// The names of the `[dependencies]` entries of a manifest that point at
    /// a path (a crate of this workspace), `[dev-dependencies]` excluded:
    /// tests are not part of the shipped binary. Textual, like
    /// `declared_version`: the shape that matters is a `path = ` inside the
    /// entry, on one line.
    fn path_dependencies(manifest: &str) -> Vec<String> {
        let mut in_dependencies = false;
        let mut found = Vec::new();
        for line in manifest.lines() {
            let line = line.trim_end_matches('\r').trim();
            if line.starts_with('[') {
                in_dependencies = line == "[dependencies]";
                continue;
            }
            if in_dependencies
                && line.contains("path =")
                && let Some((name, _)) = line.split_once('=')
            {
                found.push(name.trim().to_string());
            }
        }
        found
    }

    /// A companion's crate has NO path dependency: the root helper's code
    /// must change only when its own directory changes, which is exactly
    /// what `changed-components.sh`'s coupled-change guard watches. A shared
    /// workspace crate behind it would let a change reach the root binary
    /// unseen, and the helper would then ship under an unchanged number that
    /// a device, comparing for equality, never replaces -- while only
    /// `ritornello-install` could place it anyway.
    #[test]
    fn a_companion_depends_on_no_workspace_crate() {
        for name in SHIPPED_COMPANIONS {
            let found = path_dependencies(&crate_manifest(name));
            assert!(
                found.is_empty(),
                "{name} depends on {found:?} by path: a change to that crate would reach \
                 the root helper without the coupled-change guard noticing"
            );
        }
    }

    #[test]
    fn path_dependencies_reads_only_the_dependencies_section() {
        let manifest = "[package]\nname = \"x\"\n\n[dependencies]\n\
                        a = { path = \"../a\" }\nserde = \"1\"\n\
                        [dev-dependencies]\nb = { path = \"../b\" }\n";
        assert_eq!(path_dependencies(manifest), vec!["a".to_string()]);
        assert!(path_dependencies("[dependencies]\nserde = \"1\"\n").is_empty());
    }

    #[test]
    fn internal_crates_still_inherit() {
        for name in INTERNAL_CRATES {
            assert_eq!(
                declared_version(&crate_manifest(name)),
                None,
                "{name} declares its own version, but no archive is named \
                 after it — it must inherit the product number"
            );
        }
    }

    /// A prerelease suffix is the product's own, or there is none at all.
    ///
    /// **Why a component may not keep a beta number of its own.** The device
    /// decides by `differs`, which compares versions for *equality* and never
    /// for order. So a component shipped as `0.2.1` inside `v0.2.1-beta.1`
    /// and shipped again as `0.2.1` in the final `v0.2.1` would look
    /// identical to a device that already has the beta's bytes: nothing to
    /// do, and the tester keeps the older binary for ever, silently. A beta
    /// therefore ships what it changed under the beta's own number, which is
    /// what makes the final differ from it.
    ///
    /// The other direction is the one that would leak: a **stable** product
    /// forbids a suffix outright, so a component left at `-beta.2` cannot
    /// ride into a real release under a number that says "prerelease" to
    /// every device that reads it.
    /// Inside a prerelease, no component may already claim the number the
    /// finished release will carry.
    ///
    /// This is the one shape that strands a tester for ever, and it is not
    /// caught by the suffix rule above: a component declaring a bare `0.2.1`
    /// inside product `0.2.1-beta.1` carries no suffix at all, so that test
    /// waves it through. The device compares versions for **equality** — the
    /// tester installs the beta's `0.2.1` bytes, the finished `v0.2.1` ships
    /// its own `0.2.1`, the two strings match, and the newer binary is never
    /// fetched. Nothing later notices: the release is complete, the archive
    /// is attached, the row says up to date.
    ///
    /// A component that simply did not move — still at `0.2.0` while the
    /// product prepares `0.2.1-beta.1` — is fine and is the normal case for
    /// a narrow beta: it is not part of that delivery, so there is nothing
    /// to be stranded on.
    #[test]
    fn a_prerelease_ships_no_component_under_the_finished_number() {
        let product = product_version();
        let Some(_) = prerelease(&product) else {
            return; // a finished product: the rule above already covers it
        };
        let finished = product.split('-').next().unwrap_or(&product);
        // Companions are exempt, see `tied_to_product`: a companion that
        // happens to sit at the finished number is its own number, and it
        // moves only when the companion changes.
        let names = product_tied_crate_names();
        for name in names {
            let version = declared_version(&crate_manifest(&name))
                .unwrap_or_else(|| panic!("{name} declares no version of its own"));
            assert_ne!(
                version, finished,
                "{name} is {version} inside prerelease {product}: the finished \
                 {finished} will carry that same number, and a device compares \
                 versions for equality, so whoever installs it here keeps the \
                 beta's bytes for ever"
            );
        }
    }

    /// The same trap as `a_prerelease_ships_no_component_under_the_finished_number`,
    /// for language packs: a pack declaring the bare `0.2.1` inside product
    /// `0.2.1-beta.1` carries no suffix at all, so it would slip past a
    /// suffix check. The device compares versions for equality, so a pack
    /// installed from the beta under `0.2.1` and shipped again unmoved in the
    /// finished `v0.2.1` looks identical to a device that already has it: the
    /// newer archive -- if the finished release even carries a newer one --
    /// is never fetched.
    #[test]
    fn a_prerelease_ships_no_language_pack_under_the_finished_number() {
        let product = product_version();
        let Some(_) = prerelease(&product) else {
            return; // a finished product: the generation rule above covers it
        };
        let finished = product.split('-').next().unwrap_or(&product);
        for (lang, version) in declared_packs() {
            assert_ne!(
                version, finished,
                "language pack [{lang}] is {version} inside prerelease \
                 {product}: the finished {finished} will carry that same \
                 number, and a device compares versions for equality, so \
                 whoever installs it here keeps the beta's bytes for ever"
            );
        }
    }

    /// Every language `deploy/locales` carries has a pack declared for it,
    /// and no pack is declared for a language nothing translates. Without
    /// this, adding `de.toml` files would ship nothing and removing a
    /// language would leave a pack naming an empty archive.
    #[test]
    fn the_declared_packs_are_exactly_the_languages_on_disk() {
        let mut declared: Vec<String> = declared_packs().into_iter().map(|(l, _)| l).collect();
        declared.sort_unstable();
        declared.dedup();
        let on_disk = languages_on_disk();
        for lang in &on_disk {
            assert!(
                declared.contains(lang),
                "deploy/locales carries text for [{lang}], but \
                 deploy/language-packs.toml declares no pack for it -- that \
                 text would never reach a device"
            );
        }
        for lang in &declared {
            assert!(
                on_disk.contains(lang),
                "deploy/language-packs.toml declares a pack for [{lang}], \
                 but deploy/locales carries no text for it -- that pack \
                 would name an archive with nothing in it"
            );
        }
    }

    /// **F2 of the whole-branch review.** No declared language may be a
    /// `-`-prefix of another declared language: the `publish` job of
    /// `.github/workflows/ci.yml` keeps each changed component's archive
    /// with `mv assets/"$c"-*.tar.gz keep/`, one `mv` per name
    /// `changed-components.sh` printed. With `pt` and `pt-BR` both
    /// declared and both changed, the `mv` for `pt` also matches
    /// `pt-BR`'s archive (its glob is `pt-*.tar.gz`, and
    /// `ritornello-lang-pt-BR-0.2.1.tar.gz` fits that shape), so the first
    /// `mv` silently takes both files and the second one fails `cannot
    /// stat` -- under the Actions default shell
    /// (`bash --noprofile --norc -eo pipefail`) that failure exits the
    /// step, and if only `pt` had been the one actually needing
    /// publication, `pt-BR`'s unchanged archive would have been quietly
    /// republished under its old number, never fetched by a device.
    ///
    /// Dormant today -- exactly one language is declared -- which is
    /// exactly why this guard exists rather than waiting to be found the
    /// day a second, related language ships: none of the three guards this
    /// chantier added can see it, because each checks one language against
    /// itself.
    ///
    /// **Not anchored on a digit.** A first version of this rule tried
    /// "language, then a dash, then a digit", on the theory that a version
    /// suffix is what actually collides with the glob. `es-419` is a real
    /// BCP 47 language tag with no dash-prefix relationship to `es` at
    /// all, and it would pass that anchor by coincidence (`4` is a digit)
    /// while meaning something completely different -- the exact shape of
    /// mistake this project has already paid for once by inferring a
    /// field instead of asserting the rule it actually means: "no declared
    /// language is a longer declared language's own stem".
    #[test]
    fn no_declared_language_is_a_dash_prefix_of_another() {
        let declared: Vec<String> = declared_packs().into_iter().map(|(l, _)| l).collect();
        for a in &declared {
            for b in &declared {
                if a == b {
                    continue;
                }
                assert!(
                    !b.starts_with(&format!("{a}-")),
                    "declared languages [{a}] and [{b}]: the publish job's \
                     `mv assets/\"$c\"-*.tar.gz keep/` for [{a}] also matches \
                     [{b}]'s archive ([{b}] starts with \"{a}-\"), so keeping \
                     [{a}] then [{b}] fails the second mv, and keeping only \
                     [{a}] would republish [{b}]'s unchanged archive under its \
                     old number without a word -- see the publish job of \
                     .github/workflows/ci.yml"
                );
            }
        }
    }

    /// The same generation rule, in the other language that enforces it.
    ///
    /// `scripts/package-release.sh` names every archive of a release and
    /// re-checks the generation without cargo, because it runs in a job that
    /// has no toolchain of ours. Its check was written as `${v%.*}`, which
    /// answers `0.2` for `0.2.0` and `0.2.1-beta` for `0.2.1-beta.1` — so a
    /// prerelease shipping only the component it fixes was refused, and the
    /// same suffix written without a dot was not. The script now strips the
    /// suffix first and carries the case table; this runs it.
    ///
    /// The release job is the script's only other exercise, and it fires on
    /// a tag: without this test the table would first be read on the day a
    /// release is being cut.
    #[test]
    fn the_packaging_script_agrees_about_generations_and_prereleases() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let out = std::process::Command::new("bash")
            .arg("scripts/package-release.sh")
            .arg("--self-test")
            .current_dir(&root)
            .output()
            .expect("bash is available: the Rust suite runs on Linux here and in CI");
        assert!(
            out.status.success(),
            "package-release.sh --self-test failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// The same shape as the test above, for `changed-components.sh`'s
    /// language-pack decision and naming -- and the closer of a real gap:
    /// the `publish` job of `.github/workflows/ci.yml` keeps only
    /// `assets/"$c"-*.tar.gz` for every name `changed-components.sh` prints,
    /// then `rm -rf assets`. So this script's stdout is not informative --
    /// it decides what survives that deletion. Its own `--self-test` proves
    /// both halves of the version-diff decision (a pack whose number moved
    /// is named, one that did not is not) and, separately, that the bare
    /// token it would emit for a pack is exactly the prefix
    /// `package-release.sh` actually built an archive under -- by building
    /// one for real and checking the file exists, rather than comparing two
    /// copies of the same literal string.
    ///
    /// Run here rather than only manually, for the same reason as its
    /// neighbour above: this script's only other exercise is the `publish`
    /// job, which fires on a tag, and a workflow change is never testable
    /// from its own branch.
    #[test]
    fn changed_components_agrees_about_language_pack_naming() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let out = std::process::Command::new("bash")
            .arg("scripts/changed-components.sh")
            .arg("--self-test")
            .current_dir(&root)
            .output()
            .expect("bash is available: the Rust suite runs on Linux here and in CI");
        assert!(
            out.status.success(),
            "changed-components.sh --self-test failed:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Semver precedence of two prerelease strings (`beta.2`, `rc.1`):
    /// identifiers are dot-separated, numeric ones compare numerically,
    /// alphanumeric ones lexically, a numeric identifier sorts below an
    /// alphanumeric one, and when every shared identifier is equal the
    /// shorter list is the lower. Written by hand: `semver` is not a
    /// dependency of this crate, and one comparison does not justify one.
    fn compare_prerelease(a: &str, b: &str) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        let numeric = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
        let (mut xs, mut ys) = (a.split('.'), b.split('.'));
        loop {
            match (xs.next(), ys.next()) {
                (None, None) => return Ordering::Equal,
                (None, Some(_)) => return Ordering::Less,
                (Some(_), None) => return Ordering::Greater,
                (Some(x), Some(y)) => {
                    let ord = match (numeric(x), numeric(y)) {
                        (true, true) => {
                            // By length once leading zeros are gone, so a
                            // huge identifier cannot overflow an integer.
                            let (x, y) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                            x.len().cmp(&y.len()).then_with(|| x.cmp(y))
                        }
                        (true, false) => Ordering::Less,
                        (false, true) => Ordering::Greater,
                        (false, false) => x.cmp(y),
                    };
                    if ord != Ordering::Equal {
                        return ord;
                    }
                }
            }
        }
    }

    /// Whether a component declaring `component` may ship inside the product
    /// `product`, as far as prerelease suffixes go. (The finished-number
    /// trap is a separate rule above: a bare number carries no suffix to
    /// judge.)
    ///
    /// * A component with no suffix is always fine here.
    /// * A finished product refuses any suffix (the owner's decision: the
    ///   first finished release of a generation moves every component still
    ///   on a beta number, once).
    /// * A prerelease product accepts a suffix only on the SAME target
    ///   number, and only one that is not newer than its own.
    fn suffix_fits(product: &str, component: &str) -> Result<(), String> {
        let split = |v: &str| match v.split_once('-') {
            Some((core, pre)) => (core.to_string(), Some(pre.to_string())),
            None => (v.to_string(), None),
        };
        let (pcore, ppre) = split(product);
        let (ccore, cpre) = split(component);
        let Some(cpre) = cpre else { return Ok(()) };
        let Some(ppre) = ppre else {
            return Err(format!(
                "{component} is a prerelease number inside the stable product \
                 {product}; the final delivery must not ship a component that \
                 still says beta"
            ));
        };
        if ccore != pcore {
            return Err(format!(
                "{component} targets {ccore}, not the product's {pcore} \
                 ({product}): a prerelease suffix is only meaningful on the \
                 target number it prepares"
            ));
        }
        if compare_prerelease(&cpre, &ppre) == std::cmp::Ordering::Greater {
            return Err(format!(
                "{component} carries a suffix newer than the product's \
                 {product}: it would claim a release that does not exist yet"
            ));
        }
        Ok(())
    }

    /// A prerelease component's suffix is the product's own or an EARLIER one
    /// of the same target number.
    ///
    /// A device compares versions for equality, so an unchanged component
    /// must be able to keep its number across prereleases: forcing every
    /// component onto the product's suffix renumbered the root mount helper
    /// (`files-mount`) on every beta with no code change, and the web UI then
    /// refused to update `files` until `ritornello-install` had been run for
    /// nothing. A suffix NEWER than the product's would be a component
    /// claiming a release that does not exist yet, and a different target
    /// number (`0.2.1-beta.1` inside `0.2.0-beta.3`) names another delivery
    /// altogether. A finished product still refuses any suffix.
    #[test]
    fn a_prerelease_component_suffix_is_the_products_or_an_earlier_one() {
        let product = product_version();
        // Companions are exempt, see `tied_to_product`.
        let names = product_tied_crate_names();
        for name in names {
            let version = declared_version(&crate_manifest(&name))
                .unwrap_or_else(|| panic!("{name} declares no version of its own"));
            if let Err(why) = suffix_fits(&product, &version) {
                panic!("{name}: {why}");
            }
        }
    }

    /// The predicate itself, over a table: the test above only ever sees the
    /// numbers the repository happens to carry today.
    #[test]
    fn the_suffix_rule_over_a_case_table() {
        let cases: &[(&str, &str, bool, &str)] = &[
            ("0.2.0-beta.3", "0.2.0-beta.3", true, "equal suffix"),
            ("0.2.0-beta.3", "0.2.0-beta.2", true, "older suffix, same target"),
            ("0.2.0-beta.3", "0.2.0-beta.4", false, "newer suffix"),
            ("0.2.0-beta.3", "0.2.1-beta.1", false, "other target number"),
            ("0.2.1-beta.1", "0.2.0-beta.3", false, "an older target number is another delivery"),
            ("0.2.0", "0.2.0-beta.2", false, "a suffix in a finished product"),
            ("0.2.0", "0.2.0", true, "finished in finished"),
            ("0.2.0-beta.3", "0.2.0", true, "bare number: the finished-number test judges it"),
            ("0.2.1-beta.1", "0.2.0", true, "a component that did not move"),
            ("0.2.0-rc.1", "0.2.0-beta.3", true, "beta precedes rc"),
            ("0.2.0-beta.3", "0.2.0-rc.1", false, "rc is newer than beta"),
            ("0.2.0-beta.10", "0.2.0-beta.9", true, "numeric, not lexical: 9 < 10"),
            ("0.2.0-beta.9", "0.2.0-beta.10", false, "numeric, not lexical: 10 > 9"),
            ("0.2.0-beta.3", "0.2.0-beta", true, "fewer identifiers is lower"),
            ("0.2.0-beta", "0.2.0-beta.1", false, "more identifiers is higher"),
            ("0.2.0-beta.3", "0.2.0-1", true, "numeric identifier below alphanumeric"),
        ];
        for (product, component, ok, why) in cases {
            assert_eq!(
                suffix_fits(product, component).is_ok(),
                *ok,
                "product {product}, component {component}: {why}"
            );
        }
    }

    /// The README advertises the **latest published release**, and it does so
    /// without a number written in the file.
    ///
    /// A hand-written badge has three places to drift — the URL, its `alt`
    /// text and the prose below — and it drifted: the page read `0.2` while
    /// the product was `0.2.0`, and no test noticed. Shields reading GitHub's
    /// own release list cannot drift, and it advertises what can actually be
    /// downloaded rather than what the repository is preparing: between a
    /// version bump and a publication, those are different answers.
    ///
    /// Prereleases stay out of it on purpose — the endpoint excludes them
    /// unless asked — so the beta channel never shows up as the version of
    /// the product.
    #[test]
    fn the_readme_badge_reads_the_release_list_rather_than_a_number() {
        let readme = read(&repo_root().join("README.md"));
        assert!(
            readme.contains("img.shields.io/github/v/release/"),
            "the README's version badge must be the dynamic one, so it \
             follows the latest published release on its own"
        );
        assert!(
            !readme.contains("img.shields.io/badge/version-"),
            "the README has a version number written into a badge again; \
             that is the shape that drifted, and the dynamic endpoint exists \
             precisely so nobody has to keep it in step"
        );
        assert!(
            !readme.contains("include_prereleases"),
            "the version badge must exclude prereleases: the beta channel is \
             opt-in on the device and must not be advertised as the product's \
             version"
        );
    }

    /// The one number the prose keeps is the **generation**, and it is
    /// guarded.
    ///
    /// The sentence's subject is that the protocol and the configuration may
    /// still move before 1.0, which is a fact about the generation and not
    /// about the third number. A generation moves once per minor bump, so
    /// this is the only version-shaped text in the README that does not go
    /// stale on every delivery — and this test is what makes the once true.
    #[test]
    fn the_readme_prose_names_the_product_generation() {
        let (major, minor) = generation(&product_version());
        let readme = read(&repo_root().join("README.md"));
        let expected = format!("**{major}.{minor}.x**");
        assert!(
            readme.contains(&expected),
            "the README's status sentence must name the generation as \
             {expected}; it is the product's own {}, and the sentence is \
             about what may still change before 1.0",
            product_version()
        );
    }

    /// The list above is only worth something if it cannot fall behind the
    /// repository. `plugins.example.toml` is the same source
    /// `package-release.sh` and `deploy.sh` derive the plugin list from.
    #[test]
    fn the_shipped_plugin_list_matches_the_example_file() {
        let example = read(&repo_root().join("deploy").join("plugins.example.toml"));
        let mut declared: Vec<&str> = example
            .lines()
            .filter_map(|l| {
                l.trim_end_matches('\r')
                    .trim()
                    .strip_prefix("name = \"")?
                    .strip_suffix('"')
            })
            .collect();
        declared.sort_unstable();
        let mut expected = SHIPPED_PLUGINS.to_vec();
        expected.sort_unstable();
        assert_eq!(
            declared, expected,
            "plugins.example.toml and SHIPPED_PLUGINS disagree — a plugin \
             was added or removed without its own version being decided"
        );
    }

    /// A companion is a component of its own: it is not one of the internal
    /// crates (those inherit the product number and no archive is named after
    /// them) and not one of the plugins (`plugins.example.toml` lists what the
    /// core loads, and a helper the core never loads must not be in it).
    #[test]
    fn a_companion_is_neither_an_internal_crate_nor_a_plugin() {
        let example = read(&repo_root().join("deploy").join("plugins.example.toml"));
        for companion in SHIPPED_COMPANIONS {
            assert!(
                !INTERNAL_CRATES.contains(companion),
                "{companion} is a companion and also listed as an internal \
                 crate: it would have to inherit the product number"
            );
            assert!(
                !companion.starts_with("ritornello-plugin-"),
                "{companion} carries the plugin prefix, so every tool that \
                 scans plugins by name would take it for one"
            );
            let bare = companion.trim_start_matches("ritornello-");
            assert!(
                !example.contains(&format!("name = \"{bare}\"")),
                "{companion} appears in plugins.example.toml: a companion is \
                 not a plugin and the core must never be told to load it"
            );
            assert!(
                declared_version(&crate_manifest(companion)).is_some(),
                "{companion} inherits the product version"
            );
        }
    }
}
