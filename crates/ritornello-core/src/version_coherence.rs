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

    /// Crates that legitimately keep inheriting the product number: no
    /// archive is named after them. `ritornello-updater` is here because it
    /// travels inside the core's archive rather than as its own component.
    const INTERNAL_CRATES: &[&str] = &[
        "ritornello-proto",
        "ritornello-i18n",
        "ritornello-plugin-sdk",
        "ritornello-updater",
    ];

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

    #[test]
    fn every_shipped_component_declares_its_own_version() {
        let mut names = vec!["ritornello-core".to_string()];
        names.extend(
            SHIPPED_PLUGINS
                .iter()
                .map(|p| format!("ritornello-plugin-{p}")),
        );
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
        let product = generation(&product_version());
        let mut names = vec!["ritornello-core".to_string()];
        names.extend(
            SHIPPED_PLUGINS
                .iter()
                .map(|p| format!("ritornello-plugin-{p}")),
        );
        for name in names {
            let version = declared_version(&crate_manifest(&name))
                .unwrap_or_else(|| panic!("{name} declares no version of its own"));
            assert_eq!(
                generation(&version),
                product,
                "{name} is {version}, off the product generation {}.{}; \
                 only the third number is free",
                product.0,
                product.1
            );
        }
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
        let mut names = vec!["ritornello-core".to_string()];
        names.extend(
            SHIPPED_PLUGINS
                .iter()
                .map(|p| format!("ritornello-plugin-{p}")),
        );
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

    #[test]
    fn a_prerelease_suffix_is_the_products_own_or_absent() {
        let product = product_version();
        let expected = prerelease(&product);
        let mut names = vec!["ritornello-core".to_string()];
        names.extend(
            SHIPPED_PLUGINS
                .iter()
                .map(|p| format!("ritornello-plugin-{p}")),
        );
        for name in names {
            let version = declared_version(&crate_manifest(&name))
                .unwrap_or_else(|| panic!("{name} declares no version of its own"));
            match (prerelease(&version), expected) {
                (None, _) => {}
                (Some(theirs), Some(ours)) => assert_eq!(
                    theirs, ours,
                    "{name} is {version}, carrying a prerelease suffix that is \
                     not the product's {product}; a device compares versions \
                     for equality, so a stale beta number is a binary that is \
                     never replaced"
                ),
                (Some(_), None) => panic!(
                    "{name} is {version}, a prerelease number inside the \
                     stable product {product}; the final delivery must not \
                     ship a component that still says beta"
                ),
            }
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
}
