//! Guard over `AGENTS.md`: it routes a reader — human or coding agent — to
//! the document that answers their question, and a router that has gone
//! stale is worse than none, because it is believed.
//!
//! Two things are checked, and deliberately only two: that every document
//! under `docs/` is named there, and that every local link in the entry
//! files resolves. Nothing here judges the prose. The rules AGENTS.md
//! states are each guarded where they live — `version_coherence.rs` for the
//! numbers, `packaging_manifest.rs` for what a release carries — which is
//! the point of keeping that file a map instead of a copy.

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    fn repo_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
    }

    fn read(rel: &str) -> String {
        let p = repo_root().join(rel);
        std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{rel}: {e}"))
    }

    #[test]
    fn every_document_is_named_in_the_map() {
        // A document added to docs/ and left out of the table is a document
        // nobody is sent to. The table is the only index of them.
        let agents = read("AGENTS.md");
        let mut found = 0;
        for entry in std::fs::read_dir(repo_root().join("docs")).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().and_then(|e| e.to_str()) != Some("md") {
                continue;
            }
            let name = path.file_name().unwrap().to_string_lossy().to_string();
            assert!(
                agents.contains(&format!("docs/{name}")),
                "docs/{name} exists and AGENTS.md never mentions it"
            );
            found += 1;
        }
        assert!(found >= 4, "found only {found} documents — the walk is wrong");
    }

    /// Every `](target)` in the entry files, minus the `#anchor`, exists.
    ///
    /// Anchors themselves are not checked here: the documents cross-link
    /// heavily and a heading-slug reimplementation would be a second
    /// renderer to keep in step. What this catches is the whole file moving
    /// or being renamed, which is what actually happens.
    #[test]
    fn every_local_link_of_the_entry_files_resolves() {
        let mut checked = 0;
        for file in ["AGENTS.md", "CLAUDE.md", "README.md"] {
            let text = read(file);
            let mut rest = text.as_str();
            while let Some(open) = rest.find("](") {
                rest = &rest[open + 2..];
                let Some(close) = rest.find(')') else { break };
                let target = &rest[..close];
                rest = &rest[close + 1..];
                if target.starts_with("http") || target.starts_with('#') {
                    continue;
                }
                let path = target.split('#').next().unwrap_or(target);
                if path.is_empty() {
                    continue;
                }
                assert!(
                    repo_root().join(path).exists(),
                    "{file} links to {path}, which does not exist"
                );
                checked += 1;
            }
        }
        assert!(checked >= 5, "checked only {checked} links — the scan is wrong");
    }
}
