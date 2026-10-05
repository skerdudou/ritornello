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

use crate::update::release::{self, Offer, Origin, Published, REPO};
use crate::update::state::Installed;
use serde::Serialize;

/// How many addressable non-official repositories the device will read in one
/// check (spec §2.3). The operator cannot add past it.
pub const SOURCES_MAX: usize = 16;

/// How long one check waits for the sources it asks, all together (spec
/// §2.3). One deadline for the whole sweep, never one per source: sixteen
/// sources each allowed their own timeout would make a check that can last
/// sixteen times longer than any one of them.
pub const SOURCES_DEADLINE: std::time::Duration = std::time::Duration::from_secs(20);

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

/// What one source answered, **usably**: a source that did not answer in
/// time, or answered something that is not a release list, has no
/// `SourceAnswer` at all. `published` is the same fold as our own release
/// list's, so every rule about drafts, prereleases and asset names is the
/// one rule.
#[derive(Debug, Clone)]
pub struct SourceAnswer {
    /// Lowercased `owner/repo`, as its `SourceTarget` named it.
    pub repo: String,
    pub published: Vec<Published>,
}

/// Asks every target at once and keeps what has come back by `deadline`.
///
/// **Concurrent, under one deadline.** A source that never answers costs the
/// sweep the deadline and nothing more, and costs the check only its own
/// rows: the others are already in. A source that answers badly (`fetch`
/// gives `None`) is simply absent, never a failure of the sweep.
///
/// Answers are returned **in target order**, whatever order they arrived in,
/// so what a check concludes does not depend on which server was quicker.
///
/// Returns at once when there is no target: a device with no third-party
/// source must not wait for anything.
pub async fn query_sources<F, Fut>(targets: &[SourceTarget], fetch: F, deadline: std::time::Duration) -> Vec<SourceAnswer>
where
    F: Fn(SourceTarget) -> Fut,
    Fut: std::future::Future<Output = Option<Vec<Published>>>,
{
    use futures::StreamExt;
    let mut pending: futures::stream::FuturesUnordered<_> = targets
        .iter()
        .cloned()
        .enumerate()
        .map(|(i, t)| {
            let repo = t.repo.clone();
            let answer = fetch(t);
            async move { (i, repo, answer.await) }
        })
        .collect();
    let mut got: Vec<(usize, SourceAnswer)> = Vec::new();
    let stop = tokio::time::sleep(deadline);
    tokio::pin!(stop);
    loop {
        tokio::select! {
            // The answers first: one that is ready at the very instant the
            // deadline falls is kept rather than lost to a coin toss.
            biased;
            next = pending.next() => match next {
                Some((i, repo, Some(published))) => got.push((i, SourceAnswer { repo, published })),
                Some((_, repo, None)) => tracing::warn!("update: {repo} gave no usable answer"),
                None => break,
            },
            _ = &mut stop => {
                tracing::warn!(
                    "update: {} source(s) still silent after {} s, not checked this time",
                    pending.len(),
                    deadline.as_secs()
                );
                break;
            }
        }
    }
    // Leaving drops `pending`, and with it every request still in flight:
    // that cancellation is what bounds the sweep, since each request's own
    // timeout (`download::fetch_text`, 60 s) is three times this deadline.
    got.sort_by_key(|(i, _)| *i);
    got.into_iter().map(|(_, a)| a).collect()
}

/// What the page says per source after a check: every target asked, in
/// order, with what its answer offered — or `answered: false` for one that
/// stayed silent or answered badly. A repository the check did not ask (over
/// the limit, or not addressable) gets no report, which the page reads as
/// "never checked" rather than as "answered nothing".
pub fn reports_of(targets: &[SourceTarget], answers: &[SourceAnswer]) -> Vec<(String, SourceReport)> {
    targets
        .iter()
        .map(|t| {
            let report = match answers.iter().find(|a| a.repo == t.repo) {
                None => SourceReport { answered: false, plugins: Vec::new(), languages: Vec::new() },
                Some(answer) => {
                    let mut plugins: Vec<String> = Vec::new();
                    let mut languages: Vec<String> = Vec::new();
                    for p in &answer.published {
                        // A stranger's core, bundle or companion is not
                        // something this device would take from it, so it is
                        // not reported as on offer either.
                        let (list, item) = match &p.offer {
                            Offer::Plugin(name) => (&mut plugins, name),
                            Offer::LanguagePack(language) => (&mut languages, language),
                            Offer::Core | Offer::Bundle | Offer::Companion(_) => continue,
                        };
                        if !list.contains(item) {
                            list.push(item.clone());
                        }
                    }
                    SourceReport { answered: true, plugins, languages }
                }
            };
            (t.repo.clone(), report)
        })
        .collect()
}

/// A plugin nobody on this device owns, offered by exactly one source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FreshOffer {
    pub name: String,
    /// Lowercased `owner/repo` of the one source that offers it.
    pub repo: String,
    pub published: Published,
}

/// A name nobody owns that two or more sources offer: none of them is
/// believed, and the page names them all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    pub name: String,
    /// Every repository that offered it, lowercased and sorted.
    pub repos: Vec<String>,
}

/// A name no source may ever offer fresh, whoever owns it today.
///
/// - `core`: the core's own name, never a plugin's;
/// - a plugin that ships with a companion, and the companion itself
///   (`plugins::COMPANIONS`): only `ritornello-install` places those, together;
/// - a privileged plugin (`plugins::is_privileged`): installed by
///   `ritornello-install`, never from this page;
/// - anything the privileged installer would not accept as a bare file name
///   (`request::valid_name`): no dot, no separator, no uppercase — so no
///   name of this list can ever spell a path;
/// - a language pack's id (`ritornello-lang-…`, and `ritornello-xlang-…`
///   for a third-party pack): those name packs, and a plugin named like one
///   would make a row and a pack answer to the same name.
///
/// Today the companion plugin and the privileged plugin are the same one
/// (`files`), so those two operands overlap; both are kept because the two
/// lists answer different questions and can part. That is why the rule is
/// written over what it is given (`reserved_in`): with the real lists neither
/// operand could be shown to bite on its own.
pub fn reserved(name: &str) -> bool {
    reserved_in(name, crate::plugins::COMPANIONS, crate::plugins::is_privileged)
}

/// The prefixes of a language pack's id: ours (`langpack::store::PACK_ID_PREFIX`)
/// and a third-party pack's.
const PACK_ID_PREFIXES: [&str; 2] = [crate::langpack::store::PACK_ID_PREFIX, "ritornello-xlang-"];

/// `reserved`, over the companions and the privilege rule it is given rather
/// than the shipped ones.
fn reserved_in(name: &str, companions: &[(&str, &str)], is_privileged: impl Fn(&str) -> bool) -> bool {
    name == "core"
        || companions.iter().any(|(plugin, companion)| *plugin == name || *companion == name)
        || is_privileged(name)
        || !ritornello_updater::request::valid_name(name)
        || PACK_ID_PREFIXES.iter().any(|prefix| name.starts_with(prefix))
}

/// Which plugins the sources may offer **fresh** — to a device that has no
/// plugin of that name — and which names they contest (spec §4.1).
///
/// The security heart of the sources: a stranger's repository must never
/// replace an official or an already-installed plugin by publishing its
/// name. A name is owned, and so never offered fresh, when:
///
/// 1. our release publishes it (`Offer::Plugin` in `ours`) — it is ours;
/// 2. it is on the device (`installed`: declared or not, binary or not, any
///    origin) — it is whoever's it is, and its only update path is its own
///    announced repository (`theirs_from`), never a fresh offer;
/// 3. it is `reserved`;
///
/// and a name nobody owns that **two or more** sources offer is a
/// `Conflict` naming every one of them, sorted, with no `FreshOffer`: which
/// of two strangers is the real author is not a question this device can
/// answer, so it believes neither.
///
/// Names and repositories are compared case-insensitively, so neither a
/// capital letter nor a second spelling of one repository slips past a
/// clause. `ours` must be a fold that was actually read: an empty one would
/// make every one of our names look free (see `Checked::judge_strangers`).
/// Offers come out in the order the sources were asked.
pub fn fresh_offers(answers: &[SourceAnswer], ours: &[Published], installed: &[Installed]) -> (Vec<FreshOffer>, Vec<Conflict>) {
    let owned_by_us = |name: &str| {
        ours.iter().any(|p| matches!(&p.offer, Offer::Plugin(n) if n.eq_ignore_ascii_case(name)))
    };
    let on_the_device = |name: &str| installed.iter().any(|p| p.name.eq_ignore_ascii_case(name));
    // Every unowned name, in first-offered order, with each distinct source
    // offering it and that source's archive.
    let mut offered: Vec<(String, Vec<(String, Published)>)> = Vec::new();
    for answer in answers {
        let repo = answer.repo.to_lowercase();
        for published in &answer.published {
            let Offer::Plugin(name) = &published.offer else { continue };
            if owned_by_us(name) || on_the_device(name) || reserved(name) {
                continue;
            }
            let at = match offered.iter().position(|(n, _)| n == name) {
                Some(at) => at,
                None => {
                    offered.push((name.clone(), Vec::new()));
                    offered.len() - 1
                }
            };
            if !offered[at].1.iter().any(|(r, _)| *r == repo) {
                offered[at].1.push((repo.clone(), published.clone()));
            }
        }
    }
    let mut fresh = Vec::new();
    let mut conflicts = Vec::new();
    for (name, mut by) in offered {
        if by.len() == 1 {
            let (repo, published) = by.remove(0);
            fresh.push(FreshOffer { name, repo, published });
        } else {
            let mut repos: Vec<String> = by.into_iter().map(|(r, _)| r).collect();
            repos.sort();
            conflicts.push(Conflict { name, repos });
        }
    }
    (fresh, conflicts)
}

/// One language pack on offer, from our release or from a source (spec §5).
///
/// **No ownership rule here, unlike plugins, and deliberately** (spec §4.1):
/// several sources may publish the same language, each in its own directory,
/// because a pack's id carries its source (`langpack::store::pack_id_for`). A
/// stranger can therefore never offer a pack under our id, nor we under
/// theirs: the id is formed here from who answered, never read from what
/// they published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackOffer {
    /// The pack's id, which is also its row's name and its directory's.
    pub id: String,
    pub language: String,
    /// Lowercased `owner/repo` of the source that offers it; `None` for ours.
    pub repo: Option<String>,
    pub published: Published,
}

/// Every language pack on offer: ours first, then each source's, in the order
/// the sources were asked.
///
/// Only `Offer::LanguagePack` is read, from either list: a source's plugin,
/// core, bundle or companion is never a pack. A source answer that names our
/// own repository is skipped (the sweep never asks it, and ours has its own
/// list), as is a language `valid_locale` refuses — its id could not be a bare
/// name. An id met twice (one repository answering under two spellings) is
/// offered once, at its first position.
pub fn pack_offers(ours: &[Published], answers: &[SourceAnswer]) -> Vec<PackOffer> {
    let from_ours = ours.iter().map(|p| (None, p));
    let from_sources = answers
        .iter()
        .filter(|a| !a.repo.eq_ignore_ascii_case(REPO))
        .flat_map(|a| a.published.iter().map(move |p| (Some(a.repo.to_lowercase()), p)));
    let mut out: Vec<PackOffer> = Vec::new();
    for (repo, published) in from_ours.chain(from_sources) {
        let Offer::LanguagePack(language) = &published.offer else { continue };
        if !crate::status::valid_locale(language) {
            continue;
        }
        let id = crate::langpack::store::pack_id_for(language, repo.as_deref());
        if out.iter().any(|o| o.id == id) {
            continue;
        }
        out.push(PackOffer { id, language: language.clone(), repo, published: published.clone() });
    }
    out
}

/// The sources installed third-party language packs announce, as
/// `source_targets`/`source_rows` take them: a `https://github.com/owner/repo`
/// URL (lowercased), one per repository, in pack id order.
///
/// Read off the pack's own `pack.toml` (`PackManifest.source`), which
/// `langpack::store::inventory` has already checked against the directory's
/// id. Ours is not a pack source; neither is a `source` that does not read as
/// a GitHub repository — `inventory` lists such a pack as ours.
pub fn pack_sources(packs: &[crate::langpack::store::InstalledPack]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for pack in packs {
        let Some(repo) = release::parse_repo_url(&pack.manifest.source).map(|r| r.to_lowercase()) else { continue };
        let url = format!("https://github.com/{repo}");
        if repo != REPO && !out.contains(&url) {
            out.push(url);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(name: &str, v: &str) -> Published {
        Published {
            offer: Offer::Plugin(name.into()),
            version: v.into(),
            url: format!("https://x/{name}"),
            size: 1,
            release_tag: "t".into(),
            checksums_url: Some("https://x/SHA256SUMS".into()),
            catalogue_url: None,
        }
    }

    fn answer(repo: &str, published: Vec<Published>) -> SourceAnswer {
        SourceAnswer { repo: repo.into(), published }
    }

    #[test]
    fn a_name_nobody_owns_is_offered_with_its_source() {
        let (fresh, conflicts) = fresh_offers(&[answer("z/zed", vec![plugin("zed", "1.0.0")])], &[], &[]);
        assert_eq!(fresh.iter().map(|f| (f.name.as_str(), f.repo.as_str())).collect::<Vec<_>>(), vec![("zed", "z/zed")]);
        assert_eq!(fresh[0].published, plugin("zed", "1.0.0"), "the offer carries that source's own archive");
        assert!(conflicts.is_empty());
    }

    /// Clause (1). **[MUTATION]** drop the `ours` check: red.
    #[test]
    fn a_name_our_release_publishes_is_never_offered_by_a_stranger() {
        let (fresh, conflicts) = fresh_offers(&[answer("evil/fork", vec![plugin("radio", "9.9.9")])], &[plugin("radio", "0.2.0")], &[]);
        assert!(fresh.is_empty() && conflicts.is_empty(), "{fresh:?} {conflicts:?}");
    }

    /// Clause (2), both shapes of "on the device". **[MUTATION]** drop the
    /// `installed` check: red.
    #[test]
    fn a_name_already_on_the_device_is_never_offered_fresh() {
        let installed = vec![Installed { name: "zed".into(), declared: true, binary_present: true, version: None, repository: None }];
        let (fresh, conflicts) = fresh_offers(&[answer("z/zed", vec![plugin("zed", "1.0.0")])], &[], &installed);
        assert!(fresh.is_empty() && conflicts.is_empty(), "{fresh:?} {conflicts:?}");
        let undeclared = vec![Installed { name: "zed".into(), declared: false, binary_present: true, version: None, repository: None }];
        let (fresh, _) = fresh_offers(&[answer("z/zed", vec![plugin("zed", "1.0.0")])], &[], &undeclared);
        assert!(fresh.is_empty(), "an undeclared binary owns its name too");
        // Declared with no binary — a stopped plugin, or one whose binary is
        // missing — owns its name just as much.
        let missing = vec![Installed { name: "zed".into(), declared: true, binary_present: false, version: None, repository: None }];
        let (fresh, _) = fresh_offers(&[answer("z/zed", vec![plugin("zed", "1.0.0")])], &[], &missing);
        assert!(fresh.is_empty(), "a declaration owns its name even without a binary");
        // Whatever its case: a declared `Zed`, from wherever, owns `zed`.
        let capital = vec![Installed {
            name: "Zed".into(),
            declared: true,
            binary_present: true,
            version: None,
            repository: Some("https://github.com/someone/else".into()),
        }];
        let (fresh, _) = fresh_offers(&[answer("z/zed", vec![plugin("zed", "1.0.0")])], &[], &capital);
        assert!(fresh.is_empty(), "an installed `Zed` owns `zed`");
    }

    /// Two sources both offering an installed name: still nothing, and not a
    /// conflict either — the name is owned, so there is nothing to arbitrate.
    #[test]
    fn an_installed_name_offered_by_two_sources_is_neither_fresh_nor_a_conflict() {
        let installed = vec![Installed { name: "zed".into(), declared: true, binary_present: true, version: None, repository: None }];
        let (fresh, conflicts) = fresh_offers(
            &[answer("a/one", vec![plugin("zed", "1.0.0")]), answer("b/two", vec![plugin("zed", "1.0.0")])],
            &[],
            &installed,
        );
        assert!(fresh.is_empty() && conflicts.is_empty(), "{fresh:?} {conflicts:?}");
    }

    /// Clause (3), through `fresh_offers`. Each name is caught by one operand
    /// of `reserved` that the others do not cover — see the next test.
    #[test]
    fn a_reserved_name_is_never_offered() {
        for name in ["core", "files", "files-mount", "Zed", "../zed", "a.b", "ritornello-lang-fr", "ritornello-xlang-fr-0123456789ab"] {
            let (fresh, conflicts) = fresh_offers(&[answer("z/zed", vec![plugin(name, "1.0.0")])], &[], &[]);
            assert!(fresh.is_empty() && conflicts.is_empty(), "{name}");
        }
    }

    /// One case per operand of `reserved`, each chosen so that **only** that
    /// operand reserves it:
    ///
    /// - `core` — the core's own name, a valid bare name otherwise;
    /// - `files-mount` — a companion's name, neither a companion plugin nor
    ///   privileged, and a valid bare name;
    /// - `files` — a companion's plugin **and** privileged: the shipped
    ///   lists name the same plugin, so each of those two operands is shown
    ///   alone by the next test, over lists where they part;
    /// - `Zed`, `../zed`, `a.b`, `""` — not a name the privileged installer
    ///   would ever form a path from.
    ///
    /// And the control: an ordinary name is not reserved, or every assertion
    /// above could pass by reserving everything.
    #[test]
    fn reserved_names_each_operand_and_nothing_else() {
        for name in ["core", "files", "files-mount", "Zed", "../zed", "a.b", ""] {
            assert!(reserved(name), "{name:?} must be reserved");
        }
        for name in ["zed", "radio", "my-plugin-2"] {
            assert!(!reserved(name), "{name:?} must not be reserved");
        }
    }

    /// The two operands the shipped lists cannot tell apart (both name
    /// `files`), each shown alone over lists where they part: a companion's
    /// plugin that is not privileged, and a privileged plugin with no
    /// companion. **[MUTATION]** drop either operand: red.
    ///
    /// Over the shipped lists, both name `files`: there, the privileged
    /// operand alone cannot be shown to matter (dropping it was measured
    /// green), which is why `reserved_in` takes the lists as arguments.
    #[test]
    fn a_companion_plugin_and_a_privileged_plugin_are_each_reserved_on_their_own() {
        let companions: &[(&str, &str)] = &[("cam", "cam-helper")];
        let privileged = |n: &str| n == "root-thing";
        assert!(reserved_in("cam", companions, privileged), "a companion's plugin");
        assert!(reserved_in("cam-helper", companions, privileged), "a companion");
        assert!(reserved_in("root-thing", companions, privileged), "a privileged plugin");
        assert!(!reserved_in("zed", companions, privileged), "and nothing else");
        // `reserved` reads the shipped lists, and no other.
        assert!(!reserved_in("files", &[], |_: &str| false) && reserved("files"));
    }

    /// A plugin may not take a language pack's id, ours or a third party's.
    /// **[MUTATION]** drop either prefix: red. And the control: a name that
    /// merely contains `lang` is not reserved.
    #[test]
    fn a_language_pack_id_is_reserved_in_both_namespaces() {
        assert!(reserved("ritornello-lang-fr"));
        assert!(reserved("ritornello-xlang-pt-0123456789ab"));
        assert!(!reserved("lang-tools") && !reserved("ritornello-language"));
    }

    /// Clause (4). **[MUTATION]** keep the first offer instead of raising a
    /// conflict: red. **[MUTATION]** leave `repos` unsorted: red (the
    /// answers arrive `b` before `a`).
    #[test]
    fn two_sources_offering_one_name_offer_nothing_and_say_who() {
        let (fresh, conflicts) =
            fresh_offers(&[answer("b/two", vec![plugin("dup", "1.0.0")]), answer("a/one", vec![plugin("dup", "2.0.0")])], &[], &[]);
        assert!(fresh.is_empty());
        assert_eq!(conflicts, vec![Conflict { name: "dup".into(), repos: vec!["a/one".into(), "b/two".into()] }]);
    }

    /// A conflict does not spill onto its neighbours: a name only one of the
    /// two sources offers is still offered fresh, from that source.
    #[test]
    fn a_conflict_costs_only_its_own_name() {
        let (fresh, conflicts) = fresh_offers(
            &[answer("a/one", vec![plugin("dup", "1.0.0"), plugin("solo", "1.0.0")]), answer("b/two", vec![plugin("dup", "1.0.0")])],
            &[],
            &[],
        );
        assert_eq!(fresh.iter().map(|f| (f.name.as_str(), f.repo.as_str())).collect::<Vec<_>>(), vec![("solo", "a/one")]);
        assert_eq!(conflicts.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), vec!["dup"]);
    }

    /// One repository answering twice under two spellings is still one
    /// source: no conflict with itself, and its repo stored lowercased.
    #[test]
    fn one_repository_in_two_spellings_is_one_source_not_a_conflict() {
        let (fresh, conflicts) =
            fresh_offers(&[answer("Z/Zed", vec![plugin("zed", "1.0.0")]), answer("z/zed", vec![plugin("zed", "1.0.0")])], &[], &[]);
        assert!(conflicts.is_empty(), "{conflicts:?}");
        assert_eq!(fresh.iter().map(|f| (f.name.as_str(), f.repo.as_str())).collect::<Vec<_>>(), vec![("zed", "z/zed")]);
    }

    /// A stranger's core, bundle, companion or language pack is never a fresh
    /// plugin: only `Offer::Plugin` is judged here.
    #[test]
    fn only_plugin_offers_are_fresh_offers() {
        let mut core = plugin("x", "1.0.0");
        core.offer = Offer::Core;
        let mut companion = plugin("x", "1.0.0");
        companion.offer = Offer::Companion("zed-mount".into());
        let mut pack = plugin("x", "1.0.0");
        pack.offer = Offer::LanguagePack("pt".into());
        let (fresh, conflicts) = fresh_offers(&[answer("z/zed", vec![core, companion, pack])], &[], &[]);
        assert!(fresh.is_empty() && conflicts.is_empty(), "{fresh:?} {conflicts:?}");
    }

    fn target(repo: &str) -> SourceTarget {
        SourceTarget { repo: repo.into(), url: crate::update::release::releases_url_for(repo) }
    }

    /// The dead source's future is pending **forever**, so only the deadline
    /// can end this sweep — and run under an outer timeout, so a sweep that
    /// lost its deadline shows here as a red assertion rather than as a test
    /// that never returns.
    #[tokio::test(start_paused = true)]
    async fn a_dead_source_costs_its_own_rows_only() {
        let targets = vec![target("dead/one"), target("fast/two")];
        let started = tokio::time::Instant::now();
        let sweep = query_sources(
            &targets,
            |t| async move {
                if t.repo == "dead/one" {
                    std::future::pending::<()>().await;
                    unreachable!()
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                Some(Vec::new())
            },
            SOURCES_DEADLINE,
        );
        let answers = tokio::time::timeout(std::time::Duration::from_secs(60), sweep)
            .await
            .expect("the sweep outlived its own deadline: nothing stops it but every source answering");
        assert_eq!(answers.iter().map(|a| a.repo.as_str()).collect::<Vec<_>>(), vec!["fast/two"]);
        assert_eq!(started.elapsed(), SOURCES_DEADLINE, "the deadline, not the dead source, ended the sweep");
    }

    #[tokio::test(start_paused = true)]
    async fn sources_are_asked_together_not_one_after_the_other() {
        // Sixteen sources each taking 15 s: in sequence that is four minutes, in
        // parallel 15 s, under the 20 s deadline — so every one answers.
        let targets: Vec<SourceTarget> = (0..16).map(|i| target(&format!("o/r{i}"))).collect();
        let started = tokio::time::Instant::now();
        let answers = query_sources(
            &targets,
            |_| async {
                tokio::time::sleep(std::time::Duration::from_secs(15)).await;
                Some(Vec::new())
            },
            SOURCES_DEADLINE,
        )
        .await;
        assert_eq!(answers.len(), 16);
        assert_eq!(started.elapsed(), std::time::Duration::from_secs(15));
    }

    #[tokio::test(start_paused = true)]
    async fn answers_come_back_in_target_order_whatever_order_they_arrive_in() {
        let targets = vec![target("slow/a"), target("quick/b")];
        let answers = query_sources(
            &targets,
            |t| async move {
                let s = if t.repo == "slow/a" { 5 } else { 1 };
                tokio::time::sleep(std::time::Duration::from_secs(s)).await;
                Some(Vec::new())
            },
            SOURCES_DEADLINE,
        )
        .await;
        assert_eq!(answers.iter().map(|a| a.repo.as_str()).collect::<Vec<_>>(), vec!["slow/a", "quick/b"]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_source_that_answered_badly_is_absent_not_fatal() {
        let targets = vec![target("bad/x"), target("good/y")];
        let answers =
            query_sources(&targets, |t| async move { (t.repo == "good/y").then(Vec::new) }, SOURCES_DEADLINE).await;
        assert_eq!(answers.iter().map(|a| a.repo.as_str()).collect::<Vec<_>>(), vec!["good/y"]);
    }

    fn offered(offer: Offer) -> Published {
        Published {
            offer,
            version: "1.0.0".into(),
            url: "https://x/a".into(),
            size: 1,
            release_tag: "v1.0.0".into(),
            checksums_url: None,
            catalogue_url: None,
        }
    }

    #[test]
    fn a_report_names_what_each_asked_source_offered_and_which_stayed_silent() {
        let targets = vec![target("quiet/one"), target("busy/two")];
        let answers = vec![SourceAnswer {
            repo: "busy/two".into(),
            published: vec![
                offered(Offer::Plugin("zed".into())),
                offered(Offer::Core),
                offered(Offer::LanguagePack("pt".into())),
                offered(Offer::Plugin("zed".into())),
                offered(Offer::Plugin("alpha".into())),
            ],
        }];
        assert_eq!(
            reports_of(&targets, &answers),
            vec![
                ("quiet/one".to_string(), SourceReport { answered: false, plugins: vec![], languages: vec![] }),
                (
                    "busy/two".to_string(),
                    SourceReport {
                        answered: true,
                        plugins: vec!["zed".into(), "alpha".into()],
                        languages: vec!["pt".into()],
                    }
                ),
            ]
        );
    }

    /// A device with no third-party source — the majority — must not pay the
    /// deadline on every check.
    #[tokio::test(start_paused = true)]
    async fn no_source_means_no_wait() {
        let started = tokio::time::Instant::now();
        let answers = query_sources(&[], |_| async { Some(Vec::new()) }, SOURCES_DEADLINE).await;
        assert!(answers.is_empty());
        assert_eq!(started.elapsed(), std::time::Duration::ZERO);
    }

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

    fn pack(language: &str) -> Published {
        offered(Offer::LanguagePack(language.into()))
    }

    fn pack_ids(offers: &[PackOffer]) -> Vec<(String, Option<String>)> {
        offers.iter().map(|o| (o.id.clone(), o.repo.clone())).collect()
    }

    /// One language, two sources, two packs: ours under our id, theirs under
    /// an id formed from who answered. No ownership rule, no conflict.
    #[test]
    fn every_source_offers_its_own_pack_of_a_language_under_its_own_id() {
        use crate::langpack::store::{pack_id, third_party_pack_id};
        let offers = pack_offers(&[pack("fr")], &[answer("z/zed", vec![pack("fr")]), answer("b/bee", vec![pack("fr")])]);
        assert_eq!(
            pack_ids(&offers),
            vec![
                (pack_id("fr"), None),
                (third_party_pack_id("fr", "z/zed"), Some("z/zed".into())),
                (third_party_pack_id("fr", "b/bee"), Some("b/bee".into())),
            ]
        );
        assert!(offers.iter().all(|o| o.language == "fr"));
    }

    /// A stranger's plugin, core, bundle or companion is never a pack.
    #[test]
    fn pack_offers_never_yields_a_pack_from_a_plugin_or_core_answer() {
        let mut companion = plugin("x", "1.0.0");
        companion.offer = Offer::Companion("x-mount".into());
        let answers = [answer(
            "z/zed",
            vec![plugin("ritornello-lang-fr", "1.0.0"), offered(Offer::Core), offered(Offer::Bundle), companion],
        )];
        assert!(pack_offers(&[plugin("fr", "1.0.0"), offered(Offer::Core)], &answers).is_empty());
    }

    /// One repository under two spellings offers one pack; an answer naming
    /// our repository offers nothing of its own; a language no id can carry is
    /// skipped. **[MUTATION]** drop the `REPO` filter: the second assertion
    /// goes red (our id, carrying a repository). **[MUTATION]** drop the
    /// language filter: the third goes red.
    #[test]
    fn pack_offers_skips_a_duplicate_ours_and_an_unusable_language() {
        use crate::langpack::store::third_party_pack_id;
        let offers = pack_offers(&[], &[answer("Z/Zed", vec![pack("fr")]), answer("z/zed", vec![pack("fr")])]);
        assert_eq!(pack_ids(&offers), vec![(third_party_pack_id("fr", "z/zed"), Some("z/zed".into()))], "one repository, one pack");
        // Our repository answering as a source: no third-party row for it.
        assert!(pack_offers(&[], &[answer("Skerdudou/Ritornello", vec![pack("fr")])]).is_empty());
        assert!(pack_offers(&[], &[answer("z/zed", vec![pack("a.b"), pack(&"a".repeat(17))])]).is_empty());
    }

    fn installed_pack(language: &str, source: &str) -> crate::langpack::store::InstalledPack {
        crate::langpack::store::InstalledPack {
            id: format!("whatever-{language}"),
            manifest: ritornello_i18n::PackManifest {
                language: language.into(),
                version: "1.0.0".into(),
                source: source.into(),
                modules: vec![],
            },
            layers: vec![],
            installed_at: 0,
        }
    }

    /// What an installed pack announces, in the shape `source_targets` reads:
    /// a third party's repository, once, never ours, never an unreadable one.
    #[test]
    fn installed_third_party_packs_announce_their_source() {
        let packs = [
            installed_pack("fr", "https://github.com/skerdudou/ritornello"),
            installed_pack("fr", "https://github.com/Z/Zed"),
            installed_pack("de", "https://github.com/z/zed"),
            installed_pack("it", "not a repository"),
            installed_pack("es", "https://github.com/SKERDUDOU/Ritornello"),
        ];
        let got = pack_sources(&packs);
        assert_eq!(got, vec!["https://github.com/z/zed".to_string()]);
        assert_eq!(repos(source_targets(&[], &got, &[])), vec!["z/zed"], "and the check asks it");
    }
}
