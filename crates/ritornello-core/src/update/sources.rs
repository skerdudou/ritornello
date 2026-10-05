//! The list of repositories this device reads releases from, as the page
//! shows it (spec §2.2).
//!
//! **A read-time union, not a stored list.** Three things put a repository in
//! front of the operator, and only one of them is the operator's to undo:
//!
//! - the official repository, always first, never removable;
//! - repositories *announced* by an installed third-party plugin (or by an
//!   installed language pack): facts about what is on the device, shown
//!   read-only — removing the row would not remove the plugin;
//! - repositories the operator *added*, which are the only thing stored
//!   (`PersistedState.update_sources`).
//!
//! Storing the union would make it go stale the moment a plugin is installed
//! or uninstalled, and the stored copy would then disagree with the device.
//! So only the operator's own additions are written, and everything else is
//! recomputed on every read. A repository that is both announced and added is
//! shown once, as announced (the plugin is the reason it is read), but keeps
//! `stored: true` so the operator's own entry can still be removed.
//!
//! Repositories are compared case-insensitively and stored lowercased:
//! GitHub treats `Owner/Repo` and `owner/repo` as the same repository, so
//! comparing raw text would list one repository twice.
//!
//! Nothing here does I/O: the routes call these functions on in-memory
//! handles, and no HTTP route may block.

use crate::update::release::{self, Origin, REPO};
use crate::update::state::Installed;
use serde::Serialize;

/// How many addressable non-official repositories the device will read in one
/// check (spec §2.3). The operator cannot add past it.
pub const SOURCES_MAX: usize = 16;

/// Reads `owner/repo` or `https://github.com/owner/repo[/][.git]` and returns
/// the lowercased `owner/repo`, or `None` for anything else.
///
/// `release::parse_repo_url` does the validation (and does not lowercase); a
/// bare `owner/repo` is only given the GitHub prefix when it does not already
/// look like a URL, so `https://evil.example/github.com/a/b` is refused
/// rather than reinterpreted.
pub fn normalize_repo(input: &str) -> Option<String> {
    let input = input.trim();
    let url = if input.starts_with("https://") {
        input.to_string()
    } else {
        format!("https://github.com/{input}")
    };
    release::parse_repo_url(&url).map(|r| r.to_lowercase())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Official,
    Announced,
    Added,
}

/// What the last check learned from one repository. Filled by the check;
/// `None` on a row before any check has answered for it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceReport {
    pub answered: bool,
    pub plugins: Vec<String>,
    pub languages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceRow {
    /// Lowercased `owner/repo`; or, for an announcement this core cannot
    /// address (not GitHub, not parseable), the raw announced text.
    pub repo: String,
    pub kind: SourceKind,
    /// The plugins whose announcement named this repository, in file order.
    pub announced_by: Vec<String>,
    /// Whether the check can query it. A foreign announcement is shown and
    /// never queried.
    pub queryable: bool,
    /// Whether the operator added it, and so can remove it.
    pub stored: bool,
    pub report: Option<SourceReport>,
}

fn row(repo: String, kind: SourceKind, queryable: bool) -> SourceRow {
    SourceRow { repo, kind, announced_by: Vec::new(), queryable, stored: false, report: None }
}

/// Puts one announced repository in the list, or credits `by` on the row it
/// already has. Our own repository is not a row of its own: it is the first
/// one, always.
fn announce(rows: &mut Vec<SourceRow>, raw: &str, by: Option<&str>) {
    let (key, queryable) = match release::parse_repo_url(raw) {
        Some(repo) => (repo.to_lowercase(), true),
        None => (raw.to_string(), false),
    };
    if key == REPO {
        return;
    }
    let at = match rows.iter().position(|r| r.repo == key) {
        Some(at) => at,
        None => {
            rows.push(row(key, SourceKind::Announced, queryable));
            rows.len() - 1
        }
    };
    if let Some(by) = by
        && !rows[at].announced_by.iter().any(|n| n == by)
    {
        rows[at].announced_by.push(by.to_string());
    }
}

/// The union view. `announced` is `(plugin name, raw announced repository)`
/// in `plugins.toml` order; `pack_sources` is what installed third-party
/// language packs announce; `added` is the stored list; `reports` is what the
/// last check said per repository.
pub fn source_rows(
    announced: &[(String, Option<String>)],
    pack_sources: &[String],
    added: &[String],
    reports: &[(String, SourceReport)],
) -> Vec<SourceRow> {
    let mut rows = vec![row(REPO.to_string(), SourceKind::Official, true)];
    for (name, repo) in announced {
        if let Some(repo) = repo {
            announce(&mut rows, repo, Some(name));
        }
    }
    for repo in pack_sources {
        announce(&mut rows, repo, None);
    }
    for raw in added {
        // A hand-edited entry that does not read, or that names the official
        // repository, is skipped: the file is never trusted to be tidy.
        let Some(repo) = normalize_repo(raw) else { continue };
        if repo == REPO {
            continue;
        }
        match rows.iter().position(|r| r.repo == repo) {
            Some(at) => rows[at].stored = true,
            None => {
                let mut added_row = row(repo, SourceKind::Added, true);
                added_row.stored = true;
                rows.push(added_row);
            }
        }
    }
    for r in &mut rows {
        r.report = reports.iter().find(|(repo, _)| *repo == r.repo).map(|(_, rep)| rep.clone());
    }
    rows
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddRefusal {
    Invalid,
    Official,
    AlreadyListed,
    Full,
}

/// Decides an addition against the rows as they are now. Returns the
/// normalised repository to store.
pub fn check_add(input: &str, current_rows: &[SourceRow]) -> Result<String, AddRefusal> {
    let repo = normalize_repo(input).ok_or(AddRefusal::Invalid)?;
    if repo == REPO {
        return Err(AddRefusal::Official);
    }
    if current_rows.iter().any(|r| r.repo == repo) {
        return Err(AddRefusal::AlreadyListed);
    }
    let counted = current_rows.iter().filter(|r| r.queryable && r.kind != SourceKind::Official).count();
    if counted >= SOURCES_MAX {
        return Err(AddRefusal::Full);
    }
    Ok(repo)
}

/// One repository this check will ask, and where to ask it.
#[derive(Debug, Clone, PartialEq, Eq)]
// Removed by Task 4, which moves the check onto these.
#[cfg_attr(not(test), allow(dead_code))]
pub struct SourceTarget {
    /// Lowercased `owner/repo`, kept for the log.
    pub repo: String,
    /// Formed **here** by `release::releases_url_for`, so the one place a
    /// host is chosen stays the one place: nothing downstream composes an
    /// address, and no announced string can reach a URL template.
    pub url: String,
}

/// The repositories one check asks: announced by an installed plugin (in
/// `plugins.toml` order), then announced by an installed language pack, then
/// added by the operator (in insertion order).
///
/// Pure, and separate from the requests it feeds, because the ceiling is the
/// one thing about this list that can be wrong without any I/O being involved.
///
/// Repositories are compared case-insensitively and a repository named twice
/// is asked once, at its first position. Ours is never asked here (it has its
/// own request), and an announcement with no GitHub endpoint to address
/// consumes no slot. **Truncated to `SOURCES_MAX`, never sampled**: a stable
/// prefix means the same repositories are checked every day.
// Removed by Task 4, which moves the check onto these.
#[cfg_attr(not(test), allow(dead_code))]
pub fn source_targets(installed: &[Installed], pack_sources: &[String], added: &[String]) -> Vec<SourceTarget> {
    let announced = |raw: &str| match release::origin(Some(raw)) {
        Origin::ThirdParty(repo) => Some(repo.to_lowercase()),
        Origin::Ours | Origin::Unknown | Origin::Foreign(_) => None,
    };
    let from_plugins = installed.iter().filter_map(|p| p.repository.as_deref()).filter_map(announced);
    let from_packs = pack_sources.iter().map(String::as_str).filter_map(announced);
    let from_operator = added.iter().filter_map(|raw| normalize_repo(raw));
    let mut seen: Vec<String> = Vec::new();
    for repo in from_plugins.chain(from_packs).chain(from_operator) {
        // The second guard for the official repository: `origin` compares
        // case-sensitively, and a hand-edited `Skerdudou/Ritornello` must not
        // become a source of its own.
        if repo != REPO && !seen.contains(&repo) {
            seen.push(repo);
        }
    }
    seen.into_iter()
        .take(SOURCES_MAX)
        .map(|repo| SourceTarget { url: release::releases_url_for(&repo), repo })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tp(name: &str, repo: &str) -> Installed {
        Installed {
            name: name.into(),
            declared: true,
            binary_present: true,
            version: Some("1.0.0".into()),
            repository: Some(format!("https://github.com/{repo}")),
        }
    }

    fn repos(targets: Vec<SourceTarget>) -> Vec<String> {
        targets.into_iter().map(|t| t.repo).collect()
    }

    #[test]
    fn targets_are_announced_then_pack_sources_then_added_without_duplicates_or_ours() {
        let installed = vec![tp("radio", "skerdudou/ritornello"), tp("zed", "Z/Zed"), tp("zed2", "z/zed"), tp("alpha", "a/alpha")];
        let got = repos(source_targets(&installed, &["https://github.com/P/Packs".into()], &["b/bee".into(), "A/ALPHA".into()]));
        assert_eq!(got, vec!["z/zed", "a/alpha", "p/packs", "b/bee"]);
    }

    #[test]
    fn a_repository_added_twice_in_different_cases_is_asked_once() {
        let got = repos(source_targets(&[], &[], &["B/Bee".into(), "b/bee".into()]));
        assert_eq!(got, vec!["b/bee"]);
    }

    #[test]
    fn the_official_repository_is_never_asked_in_any_spelling() {
        let installed = vec![tp("radio", "skerdudou/ritornello")];
        let added = vec!["skerdudou/ritornello".to_string(), "Skerdudou/Ritornello".to_string()];
        assert!(source_targets(&installed, &["https://github.com/SKERDUDOU/ritornello".into()], &added).is_empty());
        // And the announcement path alone, with no operator entry to hide it.
        assert!(source_targets(&installed, &[], &[]).is_empty());
    }

    #[test]
    fn targets_are_a_stable_prefix_of_sixteen() {
        let added: Vec<String> = (0..20).map(|i| format!("o/r{i:02}")).collect();
        let got = repos(source_targets(&[], &[], &added));
        assert_eq!(got.len(), SOURCES_MAX);
        assert_eq!(got.first().map(String::as_str), Some("o/r00"));
        assert_eq!(got.last().map(String::as_str), Some("o/r15"));
    }

    #[test]
    fn a_foreign_or_unaddressable_announcement_consumes_no_slot() {
        let mut installed = vec![Installed { repository: Some("https://gitlab.com/x/y".into()), ..tp("gl", "x/y") }];
        installed.push(Installed { repository: None, ..tp("none", "x/y") });
        installed.extend((0..16).map(|i| tp(&format!("p{i}"), &format!("o/r{i}"))));
        // The slot count alone cannot tell: a foreign row in front would still
        // leave sixteen. What must hold is that sixteen *addressable* ones fill it.
        let got = repos(source_targets(&installed, &["https://gitlab.com/p/q".into()], &["not a repo".into()]));
        let expected: Vec<String> = (0..16).map(|i| format!("o/r{i}")).collect();
        assert_eq!(got, expected);
    }

    #[test]
    fn every_target_url_is_formed_on_the_api_host() {
        let got = source_targets(&[tp("zed", "z/zed")], &[], &["b/bee".into()]);
        assert_eq!(got.len(), 2);
        for t in got {
            assert_eq!(t.url, crate::update::release::releases_url_for(&t.repo));
            assert!(t.url.starts_with("https://api.github.com/repos/"), "{}", t.url);
        }
    }

    #[test]
    fn a_repository_is_read_in_both_spellings_and_stored_lowercased() {
        assert_eq!(normalize_repo("Someone/Thing").as_deref(), Some("someone/thing"));
        assert_eq!(normalize_repo("https://github.com/Someone/Thing.git").as_deref(), Some("someone/thing"));
        assert_eq!(normalize_repo("https://github.com/someone/thing/").as_deref(), Some("someone/thing"));
        for bad in ["", "thing", "a/b/c", "https://gitlab.com/a/b", "a/..", "../b", "a/b?x", "https://evil.example/github.com/a/b"] {
            assert_eq!(normalize_repo(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_official_row_is_first_and_read_only_even_with_nothing_else() {
        let rows = source_rows(&[], &[], &[], &[]);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].repo, crate::update::release::REPO);
        assert_eq!(rows[0].kind, SourceKind::Official);
        assert!(!rows[0].stored);
    }

    #[test]
    fn the_official_row_stays_unstored_and_unique_even_if_the_file_names_it() {
        let rows = source_rows(&[], &[], &["Skerdudou/Ritornello".to_string()], &[]);
        assert_eq!(rows.len(), 1);
        assert!(!rows[0].stored);
    }

    #[test]
    fn announced_rows_follow_the_plugins_in_file_order_and_name_who_announced_them() {
        let announced = vec![
            ("radio".to_string(), Some("https://github.com/skerdudou/ritornello".to_string())), // ours: not a row
            ("zed".to_string(), Some("https://github.com/Z/Zed".to_string())),
            ("alpha".to_string(), Some("https://github.com/a/alpha".to_string())),
            ("zed-extra".to_string(), Some("https://github.com/z/zed".to_string())), // same repo, other case
            ("gl".to_string(), Some("https://gitlab.com/x/y".to_string())), // foreign: shown, not queryable
            ("silent".to_string(), None),
        ];
        let rows = source_rows(&announced, &[], &[], &[]);
        let got: Vec<(&str, SourceKind, Vec<&str>, bool)> = rows
            .iter()
            .map(|r| (r.repo.as_str(), r.kind.clone(), r.announced_by.iter().map(String::as_str).collect(), r.queryable))
            .collect();
        assert_eq!(got, vec![
            ("skerdudou/ritornello", SourceKind::Official, vec![], true),
            ("z/zed", SourceKind::Announced, vec!["zed", "zed-extra"], true),
            ("a/alpha", SourceKind::Announced, vec!["alpha"], true),
            ("https://gitlab.com/x/y", SourceKind::Announced, vec!["gl"], false),
        ]);
    }

    #[test]
    fn a_pack_announced_repository_follows_the_plugins_and_is_not_credited_to_any() {
        let announced = vec![("zed".to_string(), Some("https://github.com/z/zed".to_string()))];
        let rows = source_rows(&announced, &["https://github.com/P/Pack".to_string(), "https://github.com/z/zed".to_string()], &[], &[]);
        let got: Vec<(&str, usize)> = rows.iter().map(|r| (r.repo.as_str(), r.announced_by.len())).collect();
        assert_eq!(got, vec![("skerdudou/ritornello", 0), ("z/zed", 1), ("p/pack", 0)]);
    }

    #[test]
    fn an_added_row_that_is_also_announced_is_shown_once_as_announced_and_stays_removable() {
        let announced = vec![("zed".to_string(), Some("https://github.com/z/zed".to_string()))];
        let rows = source_rows(&announced, &[], &["z/zed".to_string(), "b/bee".to_string()], &[]);
        assert_eq!(rows.iter().filter(|r| r.repo == "z/zed").count(), 1);
        let zed = rows.iter().find(|r| r.repo == "z/zed").unwrap();
        assert_eq!((zed.kind.clone(), zed.stored), (SourceKind::Announced, true));
        let bee = rows.iter().find(|r| r.repo == "b/bee").unwrap();
        assert_eq!((bee.kind.clone(), bee.stored), (SourceKind::Added, true));
    }

    #[test]
    fn an_announced_row_that_was_never_added_is_not_stored() {
        let announced = vec![("zed".to_string(), Some("https://github.com/z/zed".to_string()))];
        let rows = source_rows(&announced, &[], &[], &[]);
        assert!(!rows.iter().find(|r| r.repo == "z/zed").unwrap().stored);
    }

    #[test]
    fn a_hand_edited_entry_that_does_not_read_is_skipped() {
        let rows = source_rows(&[], &[], &["nonsense".to_string(), "Ok/Fine".to_string()], &[]);
        let got: Vec<&str> = rows.iter().map(|r| r.repo.as_str()).collect();
        assert_eq!(got, vec!["skerdudou/ritornello", "ok/fine"]);
    }

    #[test]
    fn a_report_is_attached_to_the_row_it_describes() {
        let report = SourceReport { answered: true, plugins: vec!["p".into()], languages: vec![] };
        let rows = source_rows(&[], &[], &["b/bee".to_string()], &[("b/bee".to_string(), report.clone())]);
        assert_eq!(rows[0].report, None);
        assert_eq!(rows[1].report, Some(report));
    }

    #[test]
    fn adding_refuses_each_case_by_name() {
        let rows = source_rows(&[("zed".into(), Some("https://github.com/z/zed".into()))], &[], &["b/bee".into()], &[]);
        assert_eq!(check_add("not a repo", &rows), Err(AddRefusal::Invalid));
        assert_eq!(check_add("SKERDUDOU/Ritornello", &rows), Err(AddRefusal::Official));
        assert_eq!(check_add("Z/ZED", &rows), Err(AddRefusal::AlreadyListed)); // announced
        assert_eq!(check_add("https://github.com/b/bee", &rows), Err(AddRefusal::AlreadyListed)); // added
        assert_eq!(check_add("c/sea", &rows), Ok("c/sea".to_string()));
    }

    #[test]
    fn adding_refuses_past_sixteen_queryable_sources() {
        let added: Vec<String> = (0..16).map(|i| format!("o/r{i}")).collect();
        let rows = source_rows(&[], &[], &added, &[]);
        assert_eq!(check_add("o/one-more", &rows), Err(AddRefusal::Full));
    }

    #[test]
    fn the_official_row_does_not_count_towards_the_limit() {
        let added: Vec<String> = (0..15).map(|i| format!("o/r{i}")).collect();
        let rows = source_rows(&[], &[], &added, &[]);
        assert_eq!(check_add("o/sixteenth", &rows), Ok("o/sixteenth".to_string()));
    }

    #[test]
    fn a_foreign_announcement_does_not_count_towards_the_limit() {
        let added: Vec<String> = (0..15).map(|i| format!("o/r{i}")).collect();
        let announced = vec![("gl".to_string(), Some("https://gitlab.com/x/y".to_string()))];
        let rows = source_rows(&announced, &[], &added, &[]);
        assert_eq!(check_add("o/sixteenth", &rows), Ok("o/sixteenth".to_string()));
    }

    #[test]
    fn the_same_plugin_named_twice_is_credited_once() {
        let announced = vec![
            ("zed".to_string(), Some("https://github.com/z/zed".to_string())),
            ("zed".to_string(), Some("https://github.com/z/zed".to_string())),
        ];
        let rows = source_rows(&announced, &[], &[], &[]);
        assert_eq!(rows[1].announced_by, vec!["zed".to_string()]);
    }

    #[test]
    fn a_foreign_announcement_is_shown_once_per_raw_text_and_never_queryable() {
        let announced = vec![
            ("a".to_string(), Some("https://gitlab.com/x/y".to_string())),
            ("b".to_string(), Some("https://gitlab.com/x/y".to_string())),
        ];
        let rows = source_rows(&announced, &[], &[], &[]);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].announced_by, vec!["a".to_string(), "b".to_string()]);
        assert!(!rows[1].queryable);
    }
}
