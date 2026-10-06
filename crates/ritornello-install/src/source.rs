//! Where the installer's files come from, and the one rule that holds
//! whichever source answers: every archive's SHA-256 is verified before
//! anything reads its bytes as an archive.
//!
//! Two sources exist. `GitHub` reads a published release of this project;
//! `LocalDir` reads a directory `deploy.sh` (task 16) builds instead, so a
//! device image can be produced with no network access at release time.
//! Both implement `Source`, and the caller (`main`) never needs to know
//! which one it is holding.
//!
//! **R37 — the checksums that verify an archive are not always the chosen
//! release's own.** The publish job writes one `SHA256SUMS` per release,
//! listing only the assets *that* release carries (`.github/workflows/
//! ci.yml`: `sha256sum *.tar.gz catalogue.json inventory.json`). Because an
//! unchanged component is never re-uploaded, its archive keeps living in the
//! release where it last changed — and so does the `SHA256SUMS` line that
//! verifies it. `locate` is what finds that release; an archive is always
//! verified against the `SHA256SUMS` of the release `locate` names, never
//! against the chosen release's own. `inventory.json`, by contrast, is
//! always taken from the chosen release directly and verified against that
//! same release's `SHA256SUMS` — it is written fresh for every release, so
//! there is no older one to fall back to.
//!
//! **`GitHub`'s network is a seam (`Fetch`), not a hard dependency.** Fix
//! round 1 of this task's review found that the R37 property above — the
//! whole point of `locate` existing — was proven only at the level of
//! `locate` itself, never at the level of `GitHub::archive`/
//! `GitHub::inventory`, which are the only code that actually *acts* on
//! what `locate` finds. A single wrong edit there (verifying against
//! `self.chosen()`'s `SHA256SUMS` instead of the carrier's) would leave
//! every test in the previous version of this file green. `Fetch` is the
//! fix: a one-method trait standing in for "an HTTP GET, capped", so tests
//! can hand `GitHub` a canned, in-memory implementation and drive
//! `archive`/`inventory` with the same fixed-string rigor `locate`'s own
//! tests already had, still with no socket opened anywhere in this crate's
//! test run.
//!
//! This module never imports from `ritornello-core`: it is a bin-only crate,
//! and pulling in `ritornello-core::update::release` for this would make a
//! library out of a binary for one file's worth of reuse. The pieces this
//! module needs from that model — parsing the releases list, the debug-only
//! URL override, the source guard that checks it — are reimplemented here,
//! minimally, rather than shared.

use anyhow::Context;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::own_version::{self, is_installer_tag};

/// Fixed at compile time, exactly as `ritornello-core::update::release::REPO`
/// is: this is the authenticity anchor for what a device or a workstation
/// treats as an official release.
pub const REPO: &str = "skerdudou/ritornello";

/// GitHub answers 403 to a request with no User-Agent.
const USER_AGENT: &str = concat!("ritornello-install/", env!("CARGO_PKG_VERSION"));

/// A response body read as text — the releases list, or a `SHA256SUMS`
/// file — is capped far above anything either can legitimately be and far
/// below what would trouble a workstation.
const TEXT_MAX: usize = 4 * 1024 * 1024;

/// 256 MiB per archive. The plugin bundle is the largest thing this
/// repository publishes and sits well under it; a response that exceeds it
/// is not one of ours.
pub const ARCHIVE_MAX: usize = 256 * 1024 * 1024;

/// The endpoint listing this repository's releases, newest and oldest alike,
/// one page. One hundred is the API's own maximum for a single page.
///
/// **The debug-only seam this installer offers a test.** Exactly the shape
/// `ritornello_core::update::release::TEST_RELEASES_URL_ENV` uses, under its
/// own variable name so the two do not collide when both crates are built
/// in the same debug session:
/// - `#[cfg(debug_assertions)]` means the branch below is **absent from a
///   release build** rather than merely inactive in one — this program
///   decides what is installed as root on a device, so a release binary
///   must not carry code that could be pointed at anything but GitHub.
/// - It overrides the list endpoint only. Every URL read afterwards — an
///   archive, a `SHA256SUMS` — comes from the parsed response itself, so a
///   test fixture only ever has to control the one address it is read from.
///
/// Superseded, for testing purposes, by `Fetch`: a `GitHub<FakeFetch>` in
/// this module's own tests never actually resolves this URL over the
/// network regardless of what string it is — `Fetch::get` looks it up in a
/// canned table — so this seam is not what fix round 1's tests lean on. It
/// stays, because `GitHub::new`'s production path still calls it, and a
/// real end-to-end run (task 14's territory) still needs a way to point a
/// debug binary at a fixture server instead of the real GitHub host.
#[cfg(debug_assertions)]
pub const TEST_RELEASES_URL_ENV: &str = "RITORNELLO_INSTALL_TEST_RELEASES_URL";

/// The URL this installer reads its release list from.
pub fn releases_url() -> String {
    #[cfg(debug_assertions)]
    if let Ok(url) = std::env::var(TEST_RELEASES_URL_ENV) {
        return url;
    }
    releases_url_for(REPO)
}

/// The same endpoint, for any `owner/repo`. Kept separate from `releases_url`
/// so the debug-only override never has a way to reach it: the override
/// touches the URL this program reads its own releases from, never the
/// literal `format!` this function performs.
pub fn releases_url_for(repo: &str) -> String {
    format!("https://api.github.com/repos/{repo}/releases?per_page=100")
}

/// One release of this project, as GitHub's API describes it: what
/// `parse_releases` keeps after dropping drafts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub tag: String,
    pub prerelease: bool,
    /// Always `false` here: `parse_releases` drops every draft before a
    /// `Release` is ever built, so nothing downstream can mistake one for a
    /// published release. Kept as a field, rather than dropped from the
    /// type, because it is one of the four things GitHub's wire format
    /// actually says about a release, and the wire type this reads from
    /// carries it under the same name.
    pub draft: bool,
    /// ISO-8601 UTC, kept as text: it is only ever compared, and such
    /// timestamps sort chronologically as plain strings, exactly as the
    /// core's own `update::release::Release` relies on.
    pub published_at: String,
    /// Asset name to its download URL.
    pub assets: BTreeMap<String, String>,
}

#[derive(serde::Deserialize)]
struct WireAsset {
    name: String,
    browser_download_url: String,
}

#[derive(serde::Deserialize)]
struct WireRelease {
    tag_name: String,
    /// Null on a draft, which is filtered out before this is read.
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    assets: Vec<WireAsset>,
}

/// Reads a releases-list response body into the product's releases and the
/// installer's own tags, dropping every draft.
///
/// GitHub lists drafts only to a reader with push access; this installer
/// reads with no token at all, so in practice the filter is belt and braces
/// — but it is kept, because nothing here should ever depend on which side
/// of that stopped a draft from being read.
///
/// **The installer's releases are not the product's.** `installer-vX.Y.Z` and
/// the fixed `installer` sit in the same list, published and final, and would
/// otherwise be the "newest final release" this program installs from. Their
/// tags are kept apart, for `own_version`'s notice, and nothing else reads
/// them.
fn parse_listing(json: &str) -> anyhow::Result<(Vec<Release>, Vec<String>)> {
    let wire: Vec<WireRelease> =
        serde_json::from_str(json).context("the releases list does not parse as JSON")?;
    let mut installer_tags = Vec::new();
    let mut releases = Vec::new();
    for r in wire.into_iter().filter(|r| !r.draft) {
        if is_installer_tag(&r.tag_name) {
            installer_tags.push(r.tag_name);
            continue;
        }
        releases.push(Release {
            tag: r.tag_name,
            prerelease: r.prerelease,
            draft: false,
            published_at: r.published_at.unwrap_or_default(),
            assets: r.assets.into_iter().map(|a| (a.name, a.browser_download_url)).collect(),
        });
    }
    Ok((releases, installer_tags))
}

/// The product's releases of a releases-list body: see `parse_listing`.
/// What the tests read; the program itself takes both halves from
/// `parse_listing` in one pass.
#[cfg(test)]
pub fn parse_releases(json: &str) -> anyhow::Result<Vec<Release>> {
    parse_listing(json).map(|(releases, _)| releases)
}

/// The most recently published release matching `predicate`, or `None`.
///
/// Sorted here rather than trusted from the API's own order: an ISO-8601 UTC
/// timestamp sorts chronologically as plain text, so this costs a string
/// comparison and no date parsing — the same reasoning
/// `ritornello_core::update::release::fold` documents for the same
/// comparison.
///
/// **The tie-break is deterministic, and part of the contract, not an
/// accident of `sort_by`'s stability.** Two releases published in the same
/// second (a batch republish, or a workflow rerun) would otherwise resolve
/// by whatever order the input slice happened to have them in — which is
/// GitHub's own response order, the exact thing the paragraph above says
/// this function does not trust. `then_with` on the tag itself (plain
/// string compare, the higher tag winning) makes the answer a function of
/// the two releases alone, never of which one the API listed first.
fn newest(releases: &[Release], predicate: impl Fn(&Release) -> bool) -> Option<&Release> {
    let mut candidates: Vec<&Release> = releases.iter().filter(|r| predicate(r)).collect();
    candidates.sort_by(|a, b| b.published_at.cmp(&a.published_at).then_with(|| b.tag.cmp(&a.tag)));
    candidates.into_iter().next()
}

/// Which release the installer acts on: `tag` exactly, when given; otherwise
/// the most recently published finished release, or — when none exists —
/// the most recently published prerelease.
pub fn choose<'a>(releases: &'a [Release], tag: Option<&str>) -> anyhow::Result<&'a Release> {
    if let Some(tag) = tag {
        return releases
            .iter()
            .find(|r| r.tag == tag)
            .ok_or_else(|| anyhow::anyhow!("no release tagged {tag:?}"));
    }
    if let Some(r) = newest(releases, |r| !r.prerelease) {
        return Ok(r);
    }
    newest(releases, |r| r.prerelease).ok_or_else(|| anyhow::anyhow!("no release is published"))
}

/// The release an archive named `asset` is actually verified and fetched
/// from: the most recently published release, published **no later than**
/// `chosen`, that carries an asset by this exact name.
///
/// This is R37's whole point. A component that has not changed since an
/// older release is never re-uploaded, so its archive — and the
/// `SHA256SUMS` line that verifies it — still live in that older release.
/// Searching every release without the "no later than `chosen`" bound would
/// find a *newer* release's re-upload of the same file name instead — the
/// exact shape a change to a shared crate produces, since every binary is
/// rebuilt and republished under unchanged component numbers when
/// `ritornello-proto`, `ritornello-i18n`, `ritornello-plugin-sdk` or
/// `ritornello-updater` moves. Verifying against that newer release's own
/// `SHA256SUMS` would still pass — its line names the same file, with the
/// same digest, because it is the same bytes — but it would be the wrong
/// proof: the operator asked for `chosen`, and an archive from after it is
/// not what `chosen` describes.
pub fn locate<'a>(releases: &'a [Release], chosen: &Release, asset: &str) -> Option<&'a Release> {
    newest(releases, |r| r.published_at <= chosen.published_at && r.assets.contains_key(asset))
}

/// `<hex>  <name>` per line, as `sha256sum` writes it — including its
/// binary-mode form, `<hex> *<name>`, and the case-insensitivity a checksum
/// file is otherwise entitled to (an uppercase-hex line is not malformed).
///
/// A malformed or blank line is skipped rather than fatal, exactly as
/// `ritornello_core::update::release::parse_checksums` does: a checksum file
/// that gained a stray comment must not make the whole release unreadable,
/// and a missing entry is caught later, when the archive it was for is
/// verified. A digest that is not exactly 64 hex digits falls into the same
/// "malformed" bucket. **A name listed twice with two different digests does
/// not**: that is not noise, it is the one file this installer trusts before
/// treating bytes as an archive contradicting itself, and it is refused
/// outright rather than silently resolved to "whichever line came last". The
/// same digest repeated for the same name is harmless and passes.
pub fn parse_sums(text: &str) -> anyhow::Result<BTreeMap<String, String>> {
    let mut sums: BTreeMap<String, String> = BTreeMap::new();
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let Some(digest) = parts.next() else { continue };
        let Some(name) = parts.next() else { continue };
        // `sha256sum --binary` (and some tools by default outside Linux)
        // write `<hex> *<name>` rather than `<hex>  <name>`: the marker
        // sticks directly to the name, with no space of its own.
        let name = name.strip_prefix('*').unwrap_or(name);
        if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            continue;
        }
        let digest = digest.to_ascii_lowercase();
        if let Some(existing) = sums.get(name) {
            anyhow::ensure!(
                *existing == digest,
                "SHA256SUMS lists {name:?} twice with different digests: {existing} and {digest}"
            );
            continue;
        }
        sums.insert(name.to_string(), digest);
    }
    Ok(sums)
}

fn digest_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    // `{:02x}` per byte rather than the `hex` crate: this repository does
    // not carry it, and it is one line either way. Always lowercase, which
    // is why `parse_sums` normalises the file it reads to the same case
    // rather than `verify` folding both sides at comparison time.
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Refuses `bytes` unless `sums` names `name` with exactly its digest.
///
/// A name absent from `sums` is refused by name, the same as a wrong digest
/// — both are "this installer will not trust these bytes", and the message
/// says which failure it was. The comparison itself is a plain string
/// equality: it is case-insensitive in effect because `parse_sums` already
/// normalised every digest it read to lowercase, and `digest_hex` never
/// produces anything else.
pub fn verify(sums: &BTreeMap<String, String>, name: &str, bytes: &[u8]) -> anyhow::Result<()> {
    let expected = sums
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("SHA256SUMS has no line for {name:?}"))?;
    let got = digest_hex(bytes);
    anyhow::ensure!(
        &got == expected,
        "digest mismatch for {name:?}: SHA256SUMS says {expected}, the file hashes to {got}"
    );
    Ok(())
}

/// What the installer reads files from, whichever place they come from.
///
/// `&mut self`: `GitHub` fetches the release list once, lazily, on first
/// use, and keeps it rather than asking GitHub again for every archive.
pub trait Source {
    /// What to show the operator: a release tag, or a local path.
    fn label(&self) -> String;
    /// `inventory.json`'s own text, verified.
    fn inventory(&mut self) -> anyhow::Result<String>;
    /// One archive's bytes, verified.
    fn archive(&mut self, name: &str) -> anyhow::Result<Vec<u8>>;
}

/// Whether `name` is safe to join onto a directory and read: a single path
/// component, no separator of either flavour, and not `.` or `..`.
///
/// `LocalDir` runs unprivileged on the operator's own workstation — this is
/// not the privileged boundary `crates/ritornello-updater/src/target.rs`
/// guards, and `name` is not attacker-reachable over a network here. Still
/// checked before the join, rather than left to `verify`'s hash check to
/// catch after the fact: this module's own opening line is "every archive's
/// SHA-256 is verified before anything reads its bytes as an archive", and
/// reading a file a crafted `name` pointed *outside* the chosen directory
/// (`../../etc/shadow`) is a step this installer has no business taking
/// even when the digest check downstream would go on to refuse it anyway.
fn is_bare_file_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('/')
        && !name.contains('\\')
        && !std::path::Path::new(name).is_absolute()
}

/// A local directory of archives (`--from-dir`): `deploy.sh` (task 16)
/// builds one so a device image can be produced with no network access at
/// release time.
///
/// A single `SHA256SUMS` in the directory covers `inventory.json` and every
/// archive alike — there is only one "release" here, so R37's distinction
/// between the chosen release and the one that carries an archive does not
/// arise.
pub struct LocalDir {
    pub dir: PathBuf,
}

impl LocalDir {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    fn sums(&self) -> anyhow::Result<BTreeMap<String, String>> {
        let path = self.dir.join("SHA256SUMS");
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        parse_sums(&text).with_context(|| format!("{}", path.display()))
    }

    fn read_verified(&self, name: &str) -> anyhow::Result<Vec<u8>> {
        anyhow::ensure!(is_bare_file_name(name), "{name:?} is not a bare file name");
        let path = self.dir.join(name);
        let bytes =
            std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
        let sums = self.sums()?;
        verify(&sums, name, &bytes)
            .with_context(|| format!("{}", self.dir.display()))?;
        Ok(bytes)
    }
}

impl Source for LocalDir {
    fn label(&self) -> String {
        self.dir.display().to_string()
    }

    fn inventory(&mut self) -> anyhow::Result<String> {
        let bytes = self.read_verified("inventory.json")?;
        String::from_utf8(bytes).context("inventory.json is not valid UTF-8")
    }

    fn archive(&mut self, name: &str) -> anyhow::Result<Vec<u8>> {
        self.read_verified(name)
    }
}

/// A minimal HTTP GET, capped at `cap` bytes: the one seam between `GitHub`
/// and the network.
///
/// This exists so `GitHub::archive`/`GitHub::inventory` — the code that
/// actually acts on what `locate` finds — can be driven by a test with
/// fixed, in-memory responses, the same way `locate` itself already was.
/// Before this trait existed, R37's real property ("verify against the
/// carrier's `SHA256SUMS`, never the chosen release's own") was provable
/// only at the level of `locate`'s own return value; a wrong edit to
/// `GitHub::archive` that used the right carrier for the *download* but the
/// wrong one for the *digest* left every test green. See this module's own
/// top-level doc.
pub trait Fetch {
    fn get(&mut self, url: &str, cap: u64) -> anyhow::Result<Vec<u8>>;
}

/// The production `Fetch`: a blocking `reqwest` client, one request at a
/// time. This installer runs from a workstation's terminal, one command at
/// a time, with no event loop to share.
pub struct ReqwestFetch {
    client: reqwest::blocking::Client,
}

impl ReqwestFetch {
    pub fn new() -> anyhow::Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(std::time::Duration::from_secs(60))
            .build()
            .context("building the HTTP client")?;
        Ok(Self { client })
    }
}

/// Reads a response body up to `cap` bytes, refusing anything past it
/// without reading the rest of the stream: `Read::take` stops asking the
/// underlying connection for more the moment the limit is reached, so a
/// hostile or broken server cannot make this installer buffer an unbounded
/// body before the cap is even checked.
fn read_capped(response: reqwest::blocking::Response, cap: usize) -> anyhow::Result<Vec<u8>> {
    use std::io::Read;
    let mut buf = Vec::new();
    response
        .take(cap as u64 + 1)
        .read_to_end(&mut buf)
        .context("reading the response body")?;
    anyhow::ensure!(buf.len() <= cap, "the response exceeded {cap} bytes");
    Ok(buf)
}

impl Fetch for ReqwestFetch {
    /// **An overall per-request timeout, not merely a connect one.**
    /// `connect_timeout` (set in `ReqwestFetch::new`) bounds only the
    /// TCP/TLS handshake; a server that accepts the connection and then
    /// stalls or trickles bytes (slow-loris, a half-dead CDN edge) would
    /// otherwise hang this call — and the whole installer — indefinitely,
    /// since `read_capped`'s own cap only ever trips once enough bytes
    /// actually arrive to reach it.
    ///
    /// Implemented as `RequestBuilder::timeout` (a total budget running from
    /// the request to the finished body) rather than a separate read/idle
    /// timeout: reqwest's *blocking* client exposes no such split — that is
    /// an async-client-only knob (`ClientBuilder::read_timeout`), and this
    /// installer is blocking by design (see `Fetch`'s own doc). Thirty
    /// seconds for anything capped at `TEXT_MAX` — far above what a release
    /// listing or a `SHA256SUMS` file should ever take on any real
    /// connection — and ten minutes for anything capped above it (in
    /// practice, `ARCHIVE_MAX`): long enough that a merely slow link still
    /// finishes an install, short enough that a stalled one does not hang
    /// this program forever.
    fn get(&mut self, url: &str, cap: u64) -> anyhow::Result<Vec<u8>> {
        let timeout = if cap <= TEXT_MAX as u64 {
            std::time::Duration::from_secs(30)
        } else {
            std::time::Duration::from_secs(600)
        };
        let response = self
            .client
            .get(url)
            .timeout(timeout)
            .send()
            .and_then(reqwest::blocking::Response::error_for_status)
            .with_context(|| format!("fetching {url}"))?;
        read_capped(response, cap as usize)
    }
}

/// A published GitHub release.
///
/// Generic over `Fetch` rather than holding a concrete `reqwest` client
/// directly: `ReqwestFetch` (the default, and the only type `main`
/// command ever names) is what talks to the network; this module's own
/// tests substitute a canned, in-memory `Fetch` instead, so the R37 wiring
/// in `archive`/`inventory` below can be mutation-tested with no socket.
pub struct GitHub<F: Fetch = ReqwestFetch> {
    fetch: F,
    releases: Vec<Release>,
    /// The tags of the installer's own releases, from the same list. Not
    /// releases of the product: only the newer-installer notice reads them.
    installer_tags: Vec<String>,
    /// Index into `releases`, resolved once by `choose` and kept rather than
    /// re-resolved: a fresh `choose(&self.releases, tag)` call after the
    /// list has not changed would be pure ceremony, and re-running it against
    /// a `tag: None` request would answer a *different* release the moment
    /// this process outlives someone else publishing one — mid-command, that
    /// is a source disagreeing with itself.
    chosen: usize,
    /// Each release's parsed `SHA256SUMS`, by tag, once fetched.
    sums: BTreeMap<String, BTreeMap<String, String>>,
}

impl GitHub<ReqwestFetch> {
    /// The production constructor: talks to the real GitHub API.
    pub fn new(tag: Option<&str>) -> anyhow::Result<Self> {
        Self::with_fetch(ReqwestFetch::new()?, tag)
    }
}

impl<F: Fetch> GitHub<F> {
    /// Fetches the release list once, through `fetch`, and resolves `tag`
    /// against it immediately — a `tag` that names no release is refused
    /// here, at construction, rather than on the first archive request.
    ///
    /// Generic over `F` rather than a method on `GitHub<ReqwestFetch>`
    /// alone: this is the one entry point this module's own tests use, with
    /// a canned `Fetch` in place of `ReqwestFetch`.
    fn with_fetch(mut fetch: F, tag: Option<&str>) -> anyhow::Result<Self> {
        let url = releases_url();
        let body = fetch.get(&url, TEXT_MAX as u64)?;
        let text = String::from_utf8(body).context("the releases list is not valid UTF-8")?;
        let (releases, installer_tags) = parse_listing(&text)?;
        let chosen_tag = choose(&releases, tag)?.tag.clone();
        let chosen = releases
            .iter()
            .position(|r| r.tag == chosen_tag)
            .expect("choose only ever returns a release drawn from this same list");
        Ok(Self { fetch, releases, installer_tags, chosen, sums: BTreeMap::new() })
    }

    /// What to tell the operator when the list shows an installer newer than
    /// this one, or nothing. Read from the list already fetched: no request
    /// of its own, and nothing it finds can fail a run.
    pub fn newer_installer_notice(&self) -> Option<String> {
        own_version::newer_installer_notice(self.installer_tags.iter().map(String::as_str))
    }

    fn chosen(&self) -> &Release {
        &self.releases[self.chosen]
    }

    /// Every published release, as listed once at construction: what the
    /// version screen offers.
    pub fn releases(&self) -> &[Release] {
        &self.releases
    }

    /// The tag this source acts on.
    pub fn tag(&self) -> &str {
        &self.chosen().tag
    }

    /// Acts on `tag` instead, from the list already read: the version
    /// screen's answer. A tag the list does not carry is refused, and the
    /// choice stays as it was.
    pub fn select(&mut self, tag: &str) -> anyhow::Result<()> {
        let tag = choose(&self.releases, Some(tag))?.tag.clone();
        self.chosen = self
            .releases
            .iter()
            .position(|r| r.tag == tag)
            .expect("choose only ever returns a release drawn from this same list");
        Ok(())
    }

    fn get_text(&mut self, url: &str) -> anyhow::Result<String> {
        let bytes = self.fetch.get(url, TEXT_MAX as u64)?;
        String::from_utf8(bytes).context("response is not valid UTF-8")
    }

    /// A release's `SHA256SUMS`, fetched once per release and kept: every
    /// archive one release carries is checked against the same file, and a
    /// run that fetched it again per archive only multiplied the requests
    /// (and the chances of one failing). Kept for the life of this source,
    /// which is one run: a release's assets are never replaced in place.
    fn sums_of(&mut self, release: &Release) -> anyhow::Result<BTreeMap<String, String>> {
        if let Some(sums) = self.sums.get(&release.tag) {
            return Ok(sums.clone());
        }
        let url = release
            .assets
            .get("SHA256SUMS")
            .ok_or_else(|| anyhow::anyhow!("release {} carries no SHA256SUMS", release.tag))?
            .clone();
        let sums = parse_sums(&self.get_text(&url)?)?;
        self.sums.insert(release.tag.clone(), sums.clone());
        Ok(sums)
    }
}

impl<F: Fetch> Source for GitHub<F> {
    fn label(&self) -> String {
        format!("GitHub release {}", self.chosen().tag)
    }

    /// `inventory.json` is always taken from the chosen release itself, and
    /// verified against that same release's own `SHA256SUMS` (R37): unlike
    /// an archive, it is written fresh for every release, so there is no
    /// older one to fall back to.
    fn inventory(&mut self) -> anyhow::Result<String> {
        let chosen = self.chosen().clone();
        let url = chosen
            .assets
            .get("inventory.json")
            .ok_or_else(|| anyhow::anyhow!("release {} carries no inventory.json", chosen.tag))?
            .clone();
        let sums = self.sums_of(&chosen)?;
        anyhow::ensure!(
            sums.contains_key("inventory.json"),
            "release {}'s SHA256SUMS does not list inventory.json",
            chosen.tag
        );
        let bytes = self.fetch.get(&url, TEXT_MAX as u64)?;
        verify(&sums, "inventory.json", &bytes)?;
        String::from_utf8(bytes).context("inventory.json is not valid UTF-8")
    }

    /// An archive is verified against the `SHA256SUMS` of the release that
    /// actually carries it (`locate`), never against the chosen release's
    /// own (R37) — the property `source::tests::github_tests` exercises
    /// directly, through a canned `Fetch`, rather than only through
    /// `locate`'s own pure-function tests.
    fn archive(&mut self, name: &str) -> anyhow::Result<Vec<u8>> {
        let chosen_tag = self.chosen().tag.clone();
        let carrier = locate(&self.releases, self.chosen(), name)
            .ok_or_else(|| {
                anyhow::anyhow!("no release at or before {chosen_tag} carries an archive named {name:?}")
            })?
            .clone();
        let url = carrier
            .assets
            .get(name)
            .expect("locate only ever returns a release whose assets contain this name")
            .clone();
        let sums = self.sums_of(&carrier)?;
        anyhow::ensure!(
            sums.contains_key(name),
            "release {}'s SHA256SUMS does not list {name:?}",
            carrier.tag
        );
        let bytes = self.fetch.get(&url, ARCHIVE_MAX as u64)?;
        verify(&sums, name, &bytes)?;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// **The seam's safety, asserted rather than merely true.** Mirrors
    /// `ritornello_core::update::release`'s own
    /// `the_debug_only_seam_cannot_reach_a_release_build`, narrowed to this
    /// crate's own concerns: the line that reads
    /// `RITORNELLO_INSTALL_TEST_RELEASES_URL` must be immediately preceded
    /// by `#[cfg(debug_assertions)]`, or a release build of this installer
    /// could have its release list redirected — and this program decides
    /// what is installed as root on a device.
    ///
    /// Built from concatenated pieces so this very assertion is not one of
    /// its own matches, exactly as the core's test explains for the same
    /// reason.
    #[test]
    fn the_debug_only_seam_cannot_reach_a_release_build() {
        let here = include_str!("source.rs");
        let lines: Vec<&str> = here.lines().collect();
        let read_pattern = ["std::env::var(", "TEST_RELEASES_URL_ENV", ")"].concat();
        let read_line = lines
            .iter()
            .position(|l| l.contains(&read_pattern))
            .expect("the seam's own read call must still exist, unchanged, for this guard to mean anything");
        let guard_pattern = ["#[cfg(", "debug_assertions", ")]"].concat();
        let guarded = lines[..read_line].iter().rev().take(3).any(|l| l.contains(&guard_pattern));
        assert!(
            guarded,
            "the line reading {read_pattern:?} must be immediately preceded by {guard_pattern:?}, \
             or a release build could read RITORNELLO_INSTALL_TEST_RELEASES_URL"
        );
    }

    /// One release, in the exact shape GitHub's API answers with.
    fn rel(tag: &str, published_at: &str, draft: bool, prerelease: bool, assets: &[&str]) -> String {
        let names: Vec<String> = assets
            .iter()
            .map(|n| format!(r#"{{"name":"{n}","browser_download_url":"https://x/{tag}/{n}"}}"#))
            .collect();
        format!(
            r#"{{"tag_name":"{tag}","published_at":"{published_at}","draft":{draft},"prerelease":{prerelease},"assets":[{}]}}"#,
            names.join(",")
        )
    }

    fn body(releases: &[String]) -> String {
        format!("[{}]", releases.join(","))
    }

    #[test]
    fn a_draft_is_excluded() {
        let text = body(&[
            rel("v0.3.0", "2026-09-20T10:00:00Z", true, false, &["ritornello-core-0.3.0-armv7.tar.gz"]),
            rel("v0.2.9", "2026-09-10T10:00:00Z", false, false, &["ritornello-core-0.2.9-armv7.tar.gz"]),
        ]);
        let releases = parse_releases(&text).unwrap();
        assert_eq!(releases.len(), 1, "the draft must not appear at all");
        assert_eq!(releases[0].tag, "v0.2.9");
        assert!(!releases[0].draft);
    }

    #[test]
    fn default_choice_is_the_most_recent_final_release() {
        let text = body(&[
            rel("v0.2.9", "2026-09-10T10:00:00Z", false, false, &[]),
            rel("v0.2.9-beta.2", "2026-09-15T10:00:00Z", false, true, &[]),
            rel("v0.2.8", "2026-08-01T10:00:00Z", false, false, &[]),
        ]);
        let releases = parse_releases(&text).unwrap();
        let chosen = choose(&releases, None).unwrap();
        assert_eq!(chosen.tag, "v0.2.9", "the beta is newer but a final release exists and wins");
    }

    #[test]
    fn with_no_final_release_the_most_recent_prerelease_is_chosen() {
        let text = body(&[
            rel("v0.3.0-beta.1", "2026-09-01T10:00:00Z", false, true, &[]),
            rel("v0.3.0-beta.2", "2026-09-10T10:00:00Z", false, true, &[]),
        ]);
        let releases = parse_releases(&text).unwrap();
        let chosen = choose(&releases, None).unwrap();
        assert_eq!(chosen.tag, "v0.3.0-beta.2");
    }

    #[test]
    fn an_unknown_tag_is_an_error_that_names_it() {
        let text = body(&[rel("v0.2.9", "2026-09-10T10:00:00Z", false, false, &[])]);
        let releases = parse_releases(&text).unwrap();
        let err = choose(&releases, Some("v9.9.9")).unwrap_err();
        assert!(err.to_string().contains("v9.9.9"), "{err}");
    }

    #[test]
    fn an_exact_tag_wins_over_anything_newer() {
        let text = body(&[
            rel("v0.2.9", "2026-09-10T10:00:00Z", false, false, &[]),
            rel("v0.2.8", "2026-08-01T10:00:00Z", false, false, &[]),
        ]);
        let releases = parse_releases(&text).unwrap();
        let chosen = choose(&releases, Some("v0.2.8")).unwrap();
        assert_eq!(chosen.tag, "v0.2.8");
    }

    /// A tie on `published_at` (a batch republish, or a rerun workflow) must
    /// resolve the same way regardless of the input slice's own order — the
    /// property the doc comment on `newest` claims and which a plain stable
    /// sort, on its own, would not give it.
    #[test]
    fn a_tie_on_published_at_breaks_on_the_higher_tag() {
        let same = "2026-09-10T10:00:00Z";
        let text = body(&[rel("v0.2.9", same, false, false, &[]), rel("v0.3.0", same, false, false, &[])]);
        let releases = parse_releases(&text).unwrap();
        let chosen = choose(&releases, None).unwrap();
        assert_eq!(chosen.tag, "v0.3.0", "same timestamp: the higher tag must win, whichever came first in the list");

        // And the reverse input order must answer identically — this is
        // exactly what a sort relying on stability alone would get wrong.
        let text_reversed = body(&[rel("v0.3.0", same, false, false, &[]), rel("v0.2.9", same, false, false, &[])]);
        let releases_reversed = parse_releases(&text_reversed).unwrap();
        let chosen_reversed = choose(&releases_reversed, None).unwrap();
        assert_eq!(chosen_reversed.tag, "v0.3.0");
    }

    /// **R37, the whole property.** `radio`'s archive did not change in
    /// `v0.2.9`, so the publish job never re-uploaded it — it still lives in
    /// `v0.2.8`, the release where it last changed. `locate` must find it
    /// there, never mistake `v0.2.7`'s own unrelated archive for it, and
    /// never look past `v0.2.9` into a hypothetical later release even if
    /// one existed.
    #[test]
    fn locate_finds_an_unchanged_archive_in_an_older_release() {
        let text = body(&[
            rel("v0.2.9", "2026-09-10T10:00:00Z", false, false, &["ritornello-core-0.2.9-armv7.tar.gz", "SHA256SUMS"]),
            rel(
                "v0.2.8",
                "2026-09-01T10:00:00Z",
                false,
                false,
                &["ritornello-plugin-radio-0.2.4-armv7.tar.gz", "SHA256SUMS"],
            ),
            rel("v0.2.7", "2026-08-01T10:00:00Z", false, false, &["SHA256SUMS"]),
        ]);
        let releases = parse_releases(&text).unwrap();
        let chosen = choose(&releases, Some("v0.2.9")).unwrap();
        let carrier = locate(&releases, chosen, "ritornello-plugin-radio-0.2.4-armv7.tar.gz").unwrap();
        assert_eq!(carrier.tag, "v0.2.8");
    }

    /// The other half of R37: a release published **after** the chosen one
    /// must never be consulted, even if it happens to carry an asset of the
    /// same name — the republished-under-an-unchanged-name case a shared
    /// crate's own version bump produces.
    #[test]
    fn locate_never_looks_past_the_chosen_release() {
        let text = body(&[
            rel(
                "v0.3.0",
                "2026-10-01T10:00:00Z",
                false,
                false,
                &["ritornello-plugin-radio-0.2.4-armv7.tar.gz", "SHA256SUMS"],
            ),
            rel("v0.2.9", "2026-09-10T10:00:00Z", false, false, &["ritornello-core-0.2.9-armv7.tar.gz", "SHA256SUMS"]),
            rel(
                "v0.2.8",
                "2026-09-01T10:00:00Z",
                false,
                false,
                &["ritornello-plugin-radio-0.2.4-armv7.tar.gz", "SHA256SUMS"],
            ),
        ]);
        let releases = parse_releases(&text).unwrap();
        let chosen = choose(&releases, Some("v0.2.9")).unwrap();
        let carrier = locate(&releases, chosen, "ritornello-plugin-radio-0.2.4-armv7.tar.gz").unwrap();
        assert_eq!(carrier.tag, "v0.2.8", "must find the older carrier, never the one published after the chosen release");
    }

    #[test]
    fn locate_answers_none_when_no_release_up_to_the_chosen_one_carries_the_asset() {
        let text = body(&[rel("v0.2.9", "2026-09-10T10:00:00Z", false, false, &["SHA256SUMS"])]);
        let releases = parse_releases(&text).unwrap();
        let chosen = choose(&releases, Some("v0.2.9")).unwrap();
        assert!(locate(&releases, chosen, "ritornello-plugin-radio-0.2.4-armv7.tar.gz").is_none());
    }

    #[test]
    fn verify_accepts_the_right_digest() {
        let mut sums = BTreeMap::new();
        sums.insert("thing.tar.gz".to_string(), digest_hex(b"hello"));
        assert!(verify(&sums, "thing.tar.gz", b"hello").is_ok());
    }

    #[test]
    fn verify_refuses_a_wrong_digest() {
        let mut sums = BTreeMap::new();
        sums.insert("thing.tar.gz".to_string(), digest_hex(b"hello"));
        let err = verify(&sums, "thing.tar.gz", b"tampered").unwrap_err();
        assert!(err.to_string().contains("digest mismatch"), "{err}");
    }

    #[test]
    fn verify_refuses_a_name_sha256sums_does_not_list() {
        let sums = BTreeMap::new();
        let err = verify(&sums, "thing.tar.gz", b"hello").unwrap_err();
        assert!(err.to_string().contains("thing.tar.gz"), "{err}");
    }

    #[test]
    fn parse_sums_reads_the_sha256sum_format() {
        let sums =
            parse_sums(&format!("{}  thing.tar.gz\n{}  other.tar.gz\n", digest_hex(b"a"), digest_hex(b"b")))
                .unwrap();
        assert_eq!(sums.get("thing.tar.gz"), Some(&digest_hex(b"a")));
        assert_eq!(sums.get("other.tar.gz"), Some(&digest_hex(b"b")));
    }

    /// `sha256sum --binary` writes `<hex> *<name>`, single space, asterisk
    /// stuck to the name.
    #[test]
    fn parse_sums_strips_the_binary_mode_marker() {
        let sums = parse_sums(&format!("{} *thing.tar.gz\n", digest_hex(b"a"))).unwrap();
        assert_eq!(sums.get("thing.tar.gz"), Some(&digest_hex(b"a")), "{sums:?}");
        assert!(!sums.contains_key("*thing.tar.gz"), "{sums:?}");
    }

    /// A checksum file is not required to be lowercase; the same bytes must
    /// still verify.
    #[test]
    fn parse_sums_accepts_uppercase_hex_and_it_still_verifies() {
        let lower = digest_hex(b"hello");
        let upper = lower.to_ascii_uppercase();
        let sums = parse_sums(&format!("{upper}  thing.tar.gz\n")).unwrap();
        assert_eq!(sums.get("thing.tar.gz"), Some(&lower), "stored lowercase regardless of the file's own case");
        assert!(verify(&sums, "thing.tar.gz", b"hello").is_ok());
    }

    /// A digest of the wrong length (or containing a non-hex byte) is
    /// malformed, same bucket as a blank or comment line: skipped rather
    /// than fatal.
    #[test]
    fn parse_sums_skips_a_malformed_digest() {
        let sums = parse_sums("not-a-digest  thing.tar.gz\n").unwrap();
        assert!(sums.is_empty(), "{sums:?}");
    }

    /// The same name, same digest, listed twice: harmless.
    #[test]
    fn parse_sums_accepts_the_same_name_repeated_with_the_same_digest() {
        let d = digest_hex(b"a");
        let sums = parse_sums(&format!("{d}  thing.tar.gz\n{d}  thing.tar.gz\n")).unwrap();
        assert_eq!(sums.get("thing.tar.gz"), Some(&d));
    }

    /// The same name, two different digests: this checksum file contradicts
    /// itself, and this installer must say so rather than silently keep
    /// whichever line happened to come last.
    #[test]
    fn parse_sums_refuses_the_same_name_with_two_different_digests() {
        let text = format!("{}  thing.tar.gz\n{}  thing.tar.gz\n", digest_hex(b"a"), digest_hex(b"b"));
        let err = parse_sums(&text).unwrap_err();
        assert!(err.to_string().contains("thing.tar.gz"), "{err}");
    }

    // -- LocalDir -----------------------------------------------------------

    fn write(dir: &std::path::Path, name: &str, bytes: &[u8]) {
        std::fs::write(dir.join(name), bytes).unwrap();
    }

    fn sha256sums(entries: &[(&str, &[u8])]) -> String {
        entries.iter().map(|(name, bytes)| format!("{}  {name}\n", digest_hex(bytes))).collect()
    }

    #[test]
    fn local_dir_reads_and_verifies_the_inventory_and_an_archive() {
        let tmp = tempfile::tempdir().unwrap();
        let inventory = br#"{"format":1}"#;
        let archive = b"a fake archive body";
        write(tmp.path(), "inventory.json", inventory);
        write(tmp.path(), "ritornello-core-0.2.9-armv7.tar.gz", archive);
        write(tmp.path(), "SHA256SUMS", sha256sums(&[
            ("inventory.json", inventory),
            ("ritornello-core-0.2.9-armv7.tar.gz", archive),
        ]).as_bytes());

        let mut src = LocalDir::new(tmp.path().to_path_buf());
        assert_eq!(src.label(), tmp.path().display().to_string());
        assert_eq!(src.inventory().unwrap(), String::from_utf8_lossy(inventory));
        assert_eq!(src.archive("ritornello-core-0.2.9-armv7.tar.gz").unwrap(), archive);
    }

    /// The property the brief singles out: an archive altered *after*
    /// `SHA256SUMS` was computed must be refused, not silently trusted
    /// because it was found sitting right beside a matching checksum file.
    #[test]
    fn local_dir_refuses_an_archive_altered_after_the_checksums_were_written() {
        let tmp = tempfile::tempdir().unwrap();
        let original = b"the original bytes";
        write(tmp.path(), "ritornello-core-0.2.9-armv7.tar.gz", original);
        write(
            tmp.path(),
            "SHA256SUMS",
            sha256sums(&[("ritornello-core-0.2.9-armv7.tar.gz", original)]).as_bytes(),
        );
        // Tampered after the sums were written.
        write(tmp.path(), "ritornello-core-0.2.9-armv7.tar.gz", b"tampered bytes, same length!!");

        let mut src = LocalDir::new(tmp.path().to_path_buf());
        assert!(src.archive("ritornello-core-0.2.9-armv7.tar.gz").is_err());
    }

    #[test]
    fn local_dir_refuses_an_archive_sha256sums_does_not_list() {
        let tmp = tempfile::tempdir().unwrap();
        let archive = b"a fake archive body";
        write(tmp.path(), "ritornello-core-0.2.9-armv7.tar.gz", archive);
        write(tmp.path(), "SHA256SUMS", sha256sums(&[]).as_bytes());

        let mut src = LocalDir::new(tmp.path().to_path_buf());
        assert!(src.archive("ritornello-core-0.2.9-armv7.tar.gz").is_err());
    }

    /// A name that is not a bare file name must be refused before it is ever
    /// joined onto the directory, whether or not `SHA256SUMS` would also
    /// have caught it downstream.
    #[test]
    fn local_dir_refuses_a_name_that_is_not_a_bare_file_name() {
        let tmp = tempfile::tempdir().unwrap();
        // A file genuinely readable one directory up, so a bug that only
        // "worked" because the traversal failed to find anything could not
        // slip through unnoticed.
        let outside = tmp.path().parent().expect("a temp dir has a parent");
        write(outside, "escaped.tar.gz", b"outside the chosen directory");

        let mut src = LocalDir::new(tmp.path().to_path_buf());
        for hostile in ["../escaped.tar.gz", "sub/dir.tar.gz", "..", "."] {
            let err = src.archive(hostile).unwrap_err();
            assert!(err.to_string().contains("bare file name"), "{hostile:?}: {err}");
        }
    }

    // -- GitHub / Fetch -------------------------------------------------------
    //
    // `FakeFetch` never touches a socket: `get` is a lookup in a canned
    // table keyed by URL, so `GitHub<FakeFetch>` can be driven exactly the
    // way `locate`'s own tests drive `locate` — with fixed strings, and
    // nothing else.

    struct FakeFetch {
        responses: BTreeMap<String, Vec<u8>>,
        /// Every URL asked for, in order.
        calls: Vec<String>,
    }

    impl FakeFetch {
        fn new() -> Self {
            Self { responses: BTreeMap::new(), calls: Vec::new() }
        }

        fn with(mut self, url: &str, bytes: &[u8]) -> Self {
            self.responses.insert(url.to_string(), bytes.to_vec());
            self
        }
    }

    impl Fetch for FakeFetch {
        fn get(&mut self, url: &str, cap: u64) -> anyhow::Result<Vec<u8>> {
            self.calls.push(url.to_string());
            let bytes = self
                .responses
                .get(url)
                .ok_or_else(|| anyhow::anyhow!("no canned response for {url:?}"))?
                .clone();
            anyhow::ensure!(bytes.len() as u64 <= cap, "the response exceeded {cap} bytes");
            Ok(bytes)
        }
    }

    /// One asset's canned URL, in the same tag-qualified shape `rel` builds
    /// real ones in: `https://x/<tag>/<name>`.
    fn asset_url(tag: &str, name: &str) -> String {
        format!("https://x/{tag}/{name}")
    }

    /// A `GitHub<FakeFetch>` whose release list is exactly `releases_json`
    /// (already wrapped in `[...]` by `body`, as `rel`'s own tests build
    /// it), with `tag` chosen. The releases-list URL is whatever
    /// `releases_url()` itself returns — `FakeFetch` never dials it, so it
    /// makes no difference whether the debug-only env override is set.
    fn github(releases_json: &str, tag: Option<&str>, extra: FakeFetch) -> GitHub<FakeFetch> {
        let fetch = extra.with(&releases_url(), releases_json.as_bytes());
        GitHub::with_fetch(fetch, tag).unwrap()
    }

    const RADIO: &str = "ritornello-plugin-radio-0.2.4-armv7.tar.gz";

    /// **The exact property fix round 1 asked for a test of.** `v0.2.9` is
    /// chosen; `radio`'s archive last changed in `v0.2.8` and was never
    /// re-uploaded since. `v0.2.9`'s own `SHA256SUMS` carries an entry for
    /// the very same file name with a **different, wrong** digest — the
    /// strongest possible discriminator: if `archive()` ever verified
    /// against `self.chosen()`'s sums instead of the carrier's, this would
    /// fail on a digest mismatch, not merely on a missing entry.
    #[test]
    fn archive_verifies_against_the_carriers_sums_never_the_chosen_releases() {
        let releases = body(&[
            rel("v0.2.9", "2026-09-10T10:00:00Z", false, false, &["ritornello-core-0.2.9-armv7.tar.gz", "SHA256SUMS"]),
            rel("v0.2.8", "2026-09-01T10:00:00Z", false, false, &[RADIO, "SHA256SUMS"]),
        ]);
        let archive_bytes = b"the real radio plugin archive";
        let wrong_bytes_chosen_believes_in = b"a different, unrelated file";

        let v29_sums = sha256sums(&[(RADIO, wrong_bytes_chosen_believes_in)]);
        let v28_sums = sha256sums(&[(RADIO, archive_bytes)]);

        let fetch = FakeFetch::new()
            .with(&asset_url("v0.2.9", "SHA256SUMS"), v29_sums.as_bytes())
            .with(&asset_url("v0.2.8", "SHA256SUMS"), v28_sums.as_bytes())
            .with(&asset_url("v0.2.8", RADIO), archive_bytes);
        let mut src = github(&releases, Some("v0.2.9"), fetch);

        assert_eq!(src.label(), "GitHub release v0.2.9");
        let got = src.archive(RADIO).expect("must verify against v0.2.8's own SHA256SUMS");
        assert_eq!(got, archive_bytes);
    }

    /// `inventory.json` is always taken from — and verified against — the
    /// chosen release's own `SHA256SUMS`: there is no carrier to fall back
    /// to, it is written fresh for every release.
    #[test]
    fn inventory_verifies_against_the_chosen_releases_own_sums() {
        let releases = body(&[rel(
            "v0.2.9",
            "2026-09-10T10:00:00Z",
            false,
            false,
            &["inventory.json", "SHA256SUMS"],
        )]);
        let inventory_bytes = br#"{"format":1}"#;
        let sums = sha256sums(&[("inventory.json", inventory_bytes)]);
        let fetch = FakeFetch::new()
            .with(&asset_url("v0.2.9", "SHA256SUMS"), sums.as_bytes())
            .with(&asset_url("v0.2.9", "inventory.json"), inventory_bytes);
        let mut src = github(&releases, Some("v0.2.9"), fetch);

        assert_eq!(src.inventory().unwrap(), String::from_utf8_lossy(inventory_bytes));
    }

    /// A carrier that has no `SHA256SUMS` asset at all is refused by name,
    /// not merely by an opaque failure.
    #[test]
    fn archive_refuses_a_carrier_with_no_sha256sums_by_name() {
        let releases = body(&[rel("v0.2.9", "2026-09-10T10:00:00Z", false, false, &[RADIO])]);
        let fetch = FakeFetch::new().with(&asset_url("v0.2.9", RADIO), b"bytes");
        let mut src = github(&releases, Some("v0.2.9"), fetch);

        let err = src.archive(RADIO).unwrap_err();
        assert!(err.to_string().contains("v0.2.9"), "{err}");
        assert!(err.to_string().contains("SHA256SUMS"), "{err}");
    }

    /// A carrier with a `SHA256SUMS` asset that simply does not list the
    /// archive is refused by name too.
    #[test]
    fn archive_refuses_a_carrier_whose_sums_do_not_list_it_by_name() {
        let releases = body(&[rel("v0.2.9", "2026-09-10T10:00:00Z", false, false, &[RADIO, "SHA256SUMS"])]);
        let fetch = FakeFetch::new()
            .with(&asset_url("v0.2.9", "SHA256SUMS"), sha256sums(&[]).as_bytes())
            .with(&asset_url("v0.2.9", RADIO), b"bytes");
        let mut src = github(&releases, Some("v0.2.9"), fetch);

        let err = src.archive(RADIO).unwrap_err();
        assert!(err.to_string().contains(RADIO), "{err}");
    }

    /// Bytes that arrive different from what `SHA256SUMS` says — tampered in
    /// transit, or by a compromised mirror — are refused.
    #[test]
    fn archive_refuses_bytes_that_do_not_match_the_digest() {
        let releases = body(&[rel("v0.2.9", "2026-09-10T10:00:00Z", false, false, &[RADIO, "SHA256SUMS"])]);
        let real = b"the real archive";
        let sums = sha256sums(&[(RADIO, real)]);
        let fetch = FakeFetch::new()
            .with(&asset_url("v0.2.9", "SHA256SUMS"), sums.as_bytes())
            .with(&asset_url("v0.2.9", RADIO), b"a tampered payload, not what SHA256SUMS says");
        let mut src = github(&releases, Some("v0.2.9"), fetch);

        let err = src.archive(RADIO).unwrap_err();
        assert!(err.to_string().contains("digest mismatch"), "{err}");
    }

    /// Three archives from two carriers, and the inventory of the first:
    /// each carrier's `SHA256SUMS` is fetched once, however many files it
    /// vouches for — and each one still against its own carrier's sums.
    ///
    /// **[MUTATION]**: drop the lookup at the top of `sums_of` — this test
    /// fails (v0.2.9's sums fetched three times).
    #[test]
    fn each_carrier_s_sha256sums_is_fetched_once() {
        const CORE: &str = "ritornello-core-0.2.9-armv7.tar.gz";
        const CD: &str = "ritornello-plugin-cd-0.2.9-armv7.tar.gz";
        let releases = body(&[
            rel("v0.2.9", "2026-09-10T10:00:00Z", false, false, &[CORE, CD, "inventory.json", "SHA256SUMS"]),
            rel("v0.2.8", "2026-09-01T10:00:00Z", false, false, &[RADIO, "SHA256SUMS"]),
        ]);
        let inventory = br#"{"format":1}"#;
        let v29 = sha256sums(&[(CORE, &b"core"[..]), (CD, &b"cd"[..]), ("inventory.json", &inventory[..])]);
        let v28 = sha256sums(&[(RADIO, &b"radio"[..])]);
        let fetch = FakeFetch::new()
            .with(&asset_url("v0.2.9", "SHA256SUMS"), v29.as_bytes())
            .with(&asset_url("v0.2.8", "SHA256SUMS"), v28.as_bytes())
            .with(&asset_url("v0.2.9", "inventory.json"), inventory)
            .with(&asset_url("v0.2.9", CORE), b"core")
            .with(&asset_url("v0.2.9", CD), b"cd")
            .with(&asset_url("v0.2.8", RADIO), b"radio");
        let mut src = github(&releases, Some("v0.2.9"), fetch);
        src.inventory().unwrap();
        assert_eq!(src.archive(CORE).unwrap(), b"core");
        assert_eq!(src.archive(RADIO).unwrap(), b"radio");
        assert_eq!(src.archive(CD).unwrap(), b"cd");
        let sums_calls = |tag: &str| src.fetch.calls.iter().filter(|u| **u == asset_url(tag, "SHA256SUMS")).count();
        assert_eq!(sums_calls("v0.2.9"), 1, "{:?}", src.fetch.calls);
        assert_eq!(sums_calls("v0.2.8"), 1, "{:?}", src.fetch.calls);
    }

    /// **The installer's own releases share the list and are nobody's product
    /// release.** `installer-vX.Y.Z` and the fixed `installer` are published,
    /// final, newer than the product release most of the time, and carry
    /// archives that are no component: left in the list, the default choice
    /// would be one of them (the newest final release), the version screen
    /// would offer it, and the install would then fail on a release with no
    /// inventory.
    #[test]
    fn an_installer_release_is_never_a_product_release() {
        let installer = |tag: &str| rel(tag, "2026-09-20T10:00:00Z", false, false, &["SHA256SUMS"]);
        let releases = body(&[
            installer("installer-v0.3.0"),
            installer("installer"),
            rel("v0.2.9", "2026-09-10T10:00:00Z", false, false, &["inventory.json", "SHA256SUMS"]),
            rel("v0.3.0-beta.1", "2026-09-15T10:00:00Z", false, true, &["inventory.json", "SHA256SUMS"]),
        ]);
        let src = github(&releases, None, FakeFetch::new());
        assert_eq!(src.tag(), "v0.2.9", "the newest final PRODUCT release, whatever was published after it");
        let offered: Vec<&str> = src.releases().iter().map(|r| r.tag.as_str()).collect();
        assert_eq!(offered, ["v0.2.9", "v0.3.0-beta.1"], "the version screen offers product releases only");
        let parsed = parse_releases(&releases).unwrap();
        assert!(choose(&parsed, Some("installer")).is_err());
        assert!(choose(&parsed, Some("installer-v0.3.0")).is_err());
        // With nothing but installer releases, there is no product release.
        let only = body(&[installer("installer"), installer("installer-v0.3.0")]);
        let fetch = FakeFetch::new().with(&releases_url(), only.as_bytes());
        assert!(GitHub::with_fetch(fetch, None).is_err());
    }

    /// The newer-installer notice is read from the list already fetched: no
    /// request of its own, and never a failure.
    #[test]
    fn the_newer_installer_notice_costs_no_request_and_follows_the_list() {
        let own = crate::own_version::OWN_VERSION;
        let (major, minor, _) = {
            let mut p = own.split('.').map(|n| n.parse::<u64>().unwrap());
            (p.next().unwrap(), p.next().unwrap(), p.next().unwrap())
        };
        let list = |installer_tags: &[String]| {
            let mut releases: Vec<String> = installer_tags
                .iter()
                .map(|t| rel(t, "2026-09-20T10:00:00Z", false, false, &["SHA256SUMS"]))
                .collect();
            releases.push(rel("v0.2.9", "2026-09-10T10:00:00Z", false, false, &["inventory.json", "SHA256SUMS"]));
            body(&releases)
        };
        let newer = format!("installer-v{major}.{}.0", minor + 1);
        let src = github(&list(&[newer.clone(), "installer".to_string()]), None, FakeFetch::new());
        let notice = src.newer_installer_notice().expect("a newer installer is listed");
        assert!(notice.contains(&format!("({major}.{}.0; this one is {own})", minor + 1)), "{notice}");
        assert_eq!(src.fetch.calls.len(), 1, "the list was the only request: {:?}", src.fetch.calls);

        let equal = format!("installer-v{own}");
        for tags in [vec![equal], vec!["installer".to_string()], vec![]] {
            let src = github(&list(&tags), None, FakeFetch::new());
            assert_eq!(src.newer_installer_notice(), None, "{tags:?}");
        }
        // Garbage beside nothing: still nothing, still no failure.
        let src = github(&list(&["installer-vbanana".to_string()]), None, FakeFetch::new());
        assert_eq!(src.newer_installer_notice(), None);
    }

    /// The version screen's answer moves what every later read acts on —
    /// the inventory included — and a tag the list lacks changes nothing.
    #[test]
    fn select_moves_the_chosen_release_and_refuses_an_unknown_tag() {
        let releases = body(&[
            rel("v0.2.9", "2026-09-10T10:00:00Z", false, false, &["inventory.json", "SHA256SUMS"]),
            rel("v0.2.8", "2026-09-01T10:00:00Z", false, false, &["inventory.json", "SHA256SUMS"]),
        ]);
        let old = br#"{"format":1,"old":true}"#;
        let fetch = FakeFetch::new()
            .with(&asset_url("v0.2.8", "SHA256SUMS"), sha256sums(&[("inventory.json", old)]).as_bytes())
            .with(&asset_url("v0.2.8", "inventory.json"), old);
        let mut src = github(&releases, None, fetch);
        assert_eq!(src.tag(), "v0.2.9");
        assert_eq!(src.releases().len(), 2);
        assert!(src.select("v9.9.9").is_err());
        assert_eq!(src.tag(), "v0.2.9");
        src.select("v0.2.8").unwrap();
        assert_eq!(src.label(), "GitHub release v0.2.8");
        assert_eq!(src.inventory().unwrap(), String::from_utf8_lossy(old));
    }
}
