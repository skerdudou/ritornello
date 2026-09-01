//! Resolution of the metadata of the current track.
//!
//! Two layers stack up: what the stream announces itself (the ICY header read
//! by mpv, displayed **raw**) and what a `metadata` plugin has learned. The
//! second wins over the first when it matches what is playing.
//!
//! Everything here is pure functions and methods — no socket, no router, no
//! clock: the arbitration between plugins is precisely the part where a
//! mistake is not visible to the naked eye on the device.
//!
//! The display layout, for its part, no longer lives here: it is up to the
//! display plugin to compose its lines from `PlayerState` (see
//! `ritornello-plugin-console::display::compose`).

pub use ritornello_proto::{Track, PlayerState};

use ritornello_proto::{CoverRef, Enrichment};
use serde_json::Value;
use std::collections::HashMap;

/// Origin retained for the display when it comes from the stream itself.
pub const ORIGIN_ICY: &str = "icy";

/// Origin retained when the information comes from the **tags of the played
/// file**.
///
/// `tags` and not `mpv`: the badge shown to the user must name what they are
/// looking at — where the information comes from — and not the component that
/// read it, which is an implementation detail liable to change.
pub const ORIGIN_TAGS: &str = "tags";

/// Attributes to `contributor` each of the fields that `m` already carries.
///
/// Called on a **whole** block — the winner's, the tags' or the ICY's — all
/// of whose fields come by construction from the same hand. Later fill-ins
/// are noted one by one, where they happen.
///
/// Only notes what is **filled in**: the map says "where what you see comes
/// from", not "who was consulted". An absent field therefore does not appear
/// in it, and it is `Provenance::misses` that carries the other question.
fn note_fields(m: &mut Track, contributor: &str, derived_by: Option<&str>) {
    for (name, present) in [
        ("artist", m.artist.is_some()),
        ("title", m.title.is_some()),
        ("album", m.album.is_some()),
        ("duration", m.duration_s.is_some()),
        ("year", m.year.is_some()),
        ("links", !m.links.is_empty()),
    ] {
        if present {
            m.provenance.fields.insert(name.to_string(), contributor.to_string());
            if let Some(by) = derived_by {
                m.provenance.derived.insert(name.to_string(), by.to_string());
            }
        }
    }
}

/// True for an enrichment that brings **nothing at all** — the case of a lone
/// `searched`.
///
/// Distinct from `Enrichment::is_empty`, which only speaks of the text: a
/// contributor that only found a cover is not a failure.
fn brings_nothing(e: &Enrichment) -> bool {
    e.is_empty() && e.cover.is_none() && e.year.is_none() && e.links.is_empty()
}

/// Resolution state: what is playing, what the stream says about it, what the
/// plugins say about it.
#[derive(Debug, Default)]
pub struct Metadata {
    /// Names of the `metadata` plugins in the declaration order of
    /// `plugins.toml`. **The order is the priority**: the first declared one
    /// that answered wins, and a plugin declared lower never overwrites it.
    order: Vec<String>,
    /// Opaque identity of what is playing, produced by the Source.
    identity: Option<Value>,
    /// Last ICY title seen, raw.
    icy: Option<String>,
    /// Last tags seen on the played file (artist, title, album).
    tags: Option<Track>,
    /// Enrichments matching `identity`, per plugin.
    enrichments: HashMap<String, Enrichment>,
    /// Cover declared by the Source on its channel, with its origin. The
    /// lowest tier, and yet the highest priority for the image: the
    /// `folder.jpg` placed in the directory is the one chosen by hand.
    cover_source: Option<(CoverRef, String)>,
    /// Cover embedded in the file, read by the core.
    cover_tags: Option<CoverRef>,
    /// Cache key, once the bytes are in hand. As long as it is `None`, nothing
    /// is published: the UI must never receive the URL of a broken image.
    cover_cle: Option<String>,
    /// Keys whose fetch **failed** for what is playing.
    ///
    /// A retained reference is only a promise: `selected_cover` designates it
    /// as soon as a contributor announces it, well before the bytes are in
    /// hand. Without a memory of the failure, an unkept promise nevertheless
    /// stayed preferred forever — a station URL pattern that had rusted ("a
    /// pattern that breaks yields a silence", says the design) was enough to
    /// silence `musicbrainz` for good: `known.cover` stayed true, so it
    /// searched for nothing, and it would have been outranked anyway had it
    /// spoken.
    ///
    /// Keys and not `CoverRef`s: that is what the return channel carries (see
    /// `Core::cover_arrived`), and it is also the right granularity — two
    /// contributors giving the same URL describe the same image and fail
    /// together.
    failed_covers: std::collections::HashSet<String>,
}

impl Metadata {
    pub fn new(order: Vec<String>) -> Self {
        Self { order, ..Default::default() }
    }

    /// Replaces the list of `metadata` plugins, hence the arbitration
    /// priority.
    ///
    /// **Replaces, does not add**: a `metadata` plugin announcing itself after
    /// startup must take its place from `plugins.toml`, not the last one. The
    /// list is therefore recomputed in full by `register::metadata_order`,
    /// which remains the only place where the order is decided.
    ///
    /// Enrichments already received are kept: they describe what is playing,
    /// which a plugin's arrival does not change. Those of a plugin that would
    /// leave the list simply stop being consulted — `winner` only walks
    /// `order`.
    pub fn set_order(&mut self, order: Vec<String>) {
        self.order = order;
    }

    pub fn identity(&self) -> Option<&Value> {
        self.identity.as_ref()
    }

    /// Changes what is playing. Returns `true` if the identity actually
    /// changed, in which case **the whole resolution state was reset**.
    ///
    /// Clearing the ICY and the enrichments immediately is a behavior, not a
    /// detail: leaving the previous track on screen while waiting for the next
    /// one would be more misleading than displaying nothing.
    pub fn set_identity(&mut self, identity: Option<Value>) -> bool {
        if self.identity == identity {
            return false;
        }
        self.identity = identity;
        self.icy = None;
        self.tags = None;
        self.enrichments.clear();
        self.cover_source = None;
        self.cover_tags = None;
        self.cover_cle = None;
        // Cleared with the rest of the per-track state: a failure holds for a
        // reference *of that track*. The same URL may perfectly well answer
        // for the next track — a CDN that woke up — and a list surviving the
        // identity would prevent asking for it again.
        self.failed_covers.clear();
        true
    }

    /// Retains the tags carried by the played file. Returns `true` if they
    /// bring something new — mpv republishes the `metadata` property at every
    /// track change, and sometimes identically.
    ///
    /// Like the ICY, this layer **conditions nothing on the identity**: it
    /// must work without any plugin, and without the Source having to declare
    /// anything. That is what makes it useful to any source playing a tagged
    /// file, including a future source that would know nothing of all this.
    pub fn set_tags(&mut self, track: Track) -> bool {
        if self.tags.as_ref() == Some(&track) {
            return false;
        }
        self.tags = Some(track);
        true
    }

    /// Retains the title announced by the stream. Returns `true` if it brings
    /// something new (Icecast repeats the same header throughout a track).
    ///
    /// **Conditions nothing on the identity.** That is deliberate, and the
    /// first version did it: refusing an ICY title for lack of a current
    /// identity makes the ICY layer depend on the Source's goodwill, when it
    /// must work **without any plugin**. A Source that declares no identity —
    /// a third-party plugin, or a binary not yet updated — thus deprived the
    /// device of the only layer that works on its own, silently and with
    /// nothing in the logs.
    ///
    /// It is up to the core to decide whether something is playing: it knows
    /// on its own (see `handle_icy_title`), without asking the Source
    /// anything.
    pub fn set_icy(&mut self, title: String) -> bool {
        if self.icy.as_deref() == Some(title.as_str()) {
            return false;
        }
        self.icy = Some(title);
        // The enrichments are **not** erased here, and that is an owner
        // decision: a `metadata` plugin keeps priority over the ICY under all
        // circumstances.
        //
        // An earlier version erased them, on the grounds that a new ICY title
        // proves the track has changed and that the enrichment in memory
        // describes the previous one. That is correct, but the consequence was
        // a visible alternation: at every track, the display went through the
        // ICY form (on these streams, "Title - ARTIST", sometimes just the
        // station's own name as filler) before the plugin corrected it a
        // second later.
        //
        // A trade-off owned, and it is real: at a track change, the previous
        // title stays displayed until the plugin sends its frame. Short in
        // practice — both come from the station's same automation — but
        // lasting if the plugin stops answering. A slightly late title was
        // judged preferable to a form that changes twice per track.
        true
    }

    /// Retains an enrichment if it indeed concerns what is playing. Returns
    /// `true` if it was retained (the display must then be recomposed).
    ///
    /// Two refusals, both necessary:
    /// - identity that does not match: that is the staleness safeguard,
    ///   without it a plugin's slow answer about the previous track would
    ///   overwrite the current track;
    /// - entirely empty enrichment: it counts as a non-answer, otherwise a
    ///   higher-priority plugin that recognizes the identity without knowing
    ///   anything yet would block a lower-priority plugin that, for its part,
    ///   knows.
    ///
    /// "Entirely empty" means **nothing at all**, cover included, and that is
    /// a junction where two correct mechanisms cancelled each other: this
    /// refusal predates covers and rested on `Enrichment::is_empty`, which
    /// deliberately ignores `cover` so that a cover alone does not win the
    /// arbitration of the *text*. Measured consequence: `musicbrainz`'s
    /// generic relay — which emits precisely a cover and nothing else, and
    /// which is the very reason for the protocol's re-tiering — was refused at
    /// the door, with no trace other than a `debug!`. The tagged file with no
    /// image and the radio giving text without a photo therefore stayed black.
    pub fn add(&mut self, plugin: &str, e: Enrichment) -> bool {
        // Normalization here rather than at the single call site: `is_empty`
        // only makes sense after it, and this method is public. Idempotent,
        // and the invariant becomes local instead of resting on the caller's
        // discipline.
        let e = e.cleaned();
        let Some(current) = &self.identity else {
            tracing::debug!("enrichment from {plugin} ignored: nothing playing anymore");
            return false;
        };
        if &e.identity != current {
            tracing::debug!("enrichment from {plugin} stale, ignored");
            return false;
        }
        // `is_empty()` only speaks of the **text**. The cover had already had
        // to be exempted here; the year and the links are in the same case,
        // and forgetting them would lose them silently — a contributor that
        // only brings a year would be counted as not having answered, and its
        // value thrown away before even reaching the arbitration.
        //
        // None of our plugins is in that case today: all carry text when they
        // carry a year. That is precisely why the omission would have been
        // invisible.
        // **`searched` exempts from the refusal, and it alone.** A contributor
        // that searched without finding anything has something to say —
        // "MusicBrainz has no album for this track" is not "MusicBrainz was
        // never asked", and the screen conflated the two. It nevertheless
        // enters no arbitration: without text it cannot win (`text_block`
        // discards it), without a field it fills nothing (`composed_text`
        // only reads `Some`s). It only adds a line to `Provenance::misses`.
        if e.is_empty() && e.cover.is_none() && e.year.is_none() && e.links.is_empty() && !e.searched
        {
            // **An enrichment that says nothing at all is a withdrawal**, no
            // longer only a refusal: it removes what this plugin had retained.
            //
            // "I have nothing to say about this track" was impossible to
            // express, and its absence had a cost. Refusing the frame left the
            // plugin's **previous** enrichment in place, and since a radio's
            // identity does not change from one track to the next, the previous
            // track's title and cover stayed on screen for the whole of the
            // next one. The musicbrainz plugin worked around it by emitting the
            // station's own string signed with its own name, which is what put
            // "reworked by musicbrainz" under a field it had not touched.
            //
            // Idempotent: the second time there is nothing left to remove, so
            // nothing is republished — the same guarantee as the comparison
            // just below, and for the same reason.
            //
            // **Only on a frame that really carries nothing.** A duration or a
            // position, alone, keeps being counted as no response, exactly as
            // before: those two do not make a displayable enrichment, but
            // losing one's text by sending a position would be a trap.
            if e.duration_s.is_none() && e.position_s.is_none() {
                let before = self.selected_cover();
                if self.enrichments.remove(plugin).is_none() {
                    tracing::debug!("empty enrichment from {plugin}, nothing to withdraw");
                    return false;
                }
                tracing::debug!("{plugin} withdraws its enrichment");
                if self.selected_cover() != before {
                    self.cover_cle = None;
                }
                return true;
            }
            tracing::debug!("empty enrichment from {plugin}, counted as no response");
            return false;
        }
        if !self.order.iter().any(|n| n == plugin) {
            tracing::warn!("enrichment from an undeclared metadata plugin: {plugin}");
            return false;
        }
        // Nothing new: do not signal it. A plugin that reopens its connection
        // to a remote stream re-emits the current track every time, and
        // without this comparison each repetition would cause a write towards
        // the displays and an SSE frame towards every connected browser —
        // indefinitely if the third party closes right away. `set_icy` already
        // deduplicates.
        if self.enrichments.get(plugin) == Some(&e) {
            return false;
        }
        // `enrichments` is the third input of `selected_cover`, on a par with
        // `cover_source` and `cover_tags`: an enrichment that changes the
        // retained reference (an overriding plugin answering after a
        // `fill_only`, for example) must invalidate the published key exactly
        // as `set_cover_source`/`set_cover_tags` already do, on pain of
        // republishing a stale image under the new contributor's name.
        let before = self.selected_cover();
        self.enrichments.insert(plugin.to_string(), e);
        if self.selected_cover() != before {
            self.cover_cle = None;
        }
        true
    }

    /// Name of the plugin whose enrichment is retained, if there is one.
    ///
    /// It is **the winner**, not the last to have answered: the whole order
    /// rule is justified by predictability for whoever debugs, and this is
    /// the only instrument of that debugging. A `fill_only` is excluded from
    /// it: it is a complement, not a winner, and naming it as such would point
    /// at the wrong culprit in front of a dubious display.
    pub fn winner(&self) -> Option<&str> {
        self.order
            .iter()
            .find(|p| self.enrichments.get(*p).is_some_and(|e| !e.fill_only))
            .map(String::as_str)
    }

    /// Retains the cover declared by the Source. `true` if it is news.
    pub fn set_cover_source(&mut self, c: Option<CoverRef>, origin: &str) -> bool {
        let fresh = c.map(|r| (r, origin.to_string()));
        if self.cover_source == fresh {
            return false;
        }
        self.cover_source = fresh;
        // The retained reference changed: the published key no longer
        // describes it.
        self.cover_cle = None;
        true
    }

    /// Retains the embedded cover the core extracted. `true` if new.
    pub fn set_cover_tags(&mut self, c: Option<CoverRef>) -> bool {
        if self.cover_tags == c {
            return false;
        }
        self.cover_tags = c;
        self.cover_cle = None;
        true
    }

    /// The cover that wins, and who supplied it.
    ///
    /// The order is not an arbitrary priority list: it follows from the tiers
    /// and the intentions. The Source first — the file placed in the
    /// directory is the image chosen by hand. The core next, which
    /// **complements**: it does not replace what the Source said, and that is
    /// what gives the `folder.jpg` its precedence without any convention
    /// having to be inverted. The plugins last, in declaration order, a
    /// `fill_only` taking nobody's place.
    ///
    /// A reference whose fetch **failed** is skipped, at its tier as at the
    /// others (see `failed_covers`): precedence says whom we prefer, it does
    /// not say to prefer indefinitely an image the device failed to obtain.
    pub fn selected_cover(&self) -> Option<(CoverRef, String)> {
        if let Some((r, o)) = &self.cover_source
            && !self.has_failed(r)
        {
            return Some((r.clone(), o.clone()));
        }
        if let Some(r) = &self.cover_tags
            && !self.has_failed(r)
        {
            return Some((r.clone(), ORIGIN_TAGS.to_string()));
        }
        // An overriding plugin first, then a `fill_only`. Two passes rather
        // than one: otherwise a `fill_only` declared high in `plugins.toml`
        // would go ahead of a specialized plugin declared lower, which is
        // exactly the opposite of its intention.
        for fill_only in [false, true] {
            for plugin in &self.order {
                if let Some(e) = self.enrichments.get(plugin)
                    && e.fill_only == fill_only
                    && let Some(r) = &e.cover
                    && !self.has_failed(r)
                {
                    return Some((r.clone(), plugin.clone()));
                }
            }
        }
        None
    }

    /// Has this reference already failed for what is playing?
    fn has_failed(&self, r: &CoverRef) -> bool {
        !self.failed_covers.is_empty()
            && self.failed_covers.contains(&crate::cover::key(r))
    }

    /// Notes that a fetch failed for this key. Returns `true` if the retained
    /// reference changed because of it — the caller must then relaunch a
    /// fetch and republish.
    ///
    /// The core learns of the failure on its return channel
    /// (`succes == false`), and that is the only place it learns it: without
    /// this note, it would redesignate the same dead reference at every pass.
    pub fn mark_cover_failed(&mut self, key: String) -> bool {
        let before = self.selected_cover();
        if !self.failed_covers.insert(key) {
            return false;
        }
        let after = self.selected_cover();
        if after != before {
            // Same reason as elsewhere: the published key no longer describes
            // the retained reference, and leaving it would display one
            // contributor's image under another's name.
            self.cover_cle = None;
            return true;
        }
        false
    }

    /// Publishes the cache key. `None` = nothing left to show.
    pub fn set_cover_href(&mut self, key: Option<String>) {
        self.cover_cle = key;
    }

    /// Key already published, if there is one. Serves `Core::start_cover_fetch`
    /// to avoid relaunching a fetch whose result is already on screen — a
    /// retained enrichment that republishes identically (a station
    /// reconfirming its metadata every thirty seconds, for example) must not
    /// relaunch a task for work already done.
    pub fn published_cover(&self) -> Option<&str> {
        self.cover_cle.as_deref()
    }

    /// What is already known, as a contributor needs to see it.
    ///
    /// `cover` says a cover is **held**, never which one: a contributor does
    /// not need the image to decide whether it should look for one.
    ///
    /// "Held" means *a retained reference whose fetch has not failed* — it is
    /// `selected_cover` that discards the failed ones, so this boolean becomes
    /// false again as soon as a promised reference turns out dead. That is
    /// what makes the documentation's promise true: "for lack of a cover,
    /// `musicbrainz` complements from the artist and album this plugin just
    /// provided".
    ///
    /// Goes through `composed_text()` rather than `state()`: the latter also
    /// computes `cover_href`/`cover_origin`, which a plugin must never see
    /// (see the doc of `Known`), and would recompute the cover a second time
    /// after the one done here for `cover`.
    pub fn known(&self) -> ritornello_proto::Known {
        let m = self.composed_text();
        ritornello_proto::Known {
            artist: m.artist,
            title: m.title,
            album: m.album,
            duration_s: m.duration_s,
            year: m.year,
            cover: self.selected_cover().is_some(),
            // Verbatim, and from `self.icy` and not from `m`: `m` is the
            // **composed** text, where the ICY only appears as a last resort.
            stream_title: self.icy.clone(),
        }
    }

    /// Resolution, in order: the retained contributor's text block,
    /// complemented by the `fill_only`s, plus the retained cover if the bytes
    /// are in hand.
    pub fn state(&self) -> Track {
        let mut m = self.composed_text();
        // Only compute `selected_cover()` — a walk of `order`, a clone of a
        // `CoverRef` and of its origin — if there is a key to publish:
        // without a key, no cover reaches the display anyway (see
        // `set_cover_href`), and this method is called at least once per
        // second as long as a track is playing.
        if let Some(key) = &self.cover_cle
            && let Some((_, origin)) = self.selected_cover()
        {
            m.cover_href = Some(format!("{}{key}", crate::cover::HREF_PREFIX));
            m.provenance.fields.insert("cover".into(), origin.clone());
            m.cover_origin = Some(origin);
        }
        m
    }

    /// The composed text: the retained contributor's block (see
    /// `text_block`), complemented by the `fill_only`s. Without the cover —
    /// `state()` adds it for the display, `known()` does not need it (see its
    /// documentation).
    ///
    /// The `fill_only`s fill the block's gaps, without ever contradicting it.
    /// We do not compose field by field between two overriding contributors:
    /// that would mix two readings of the same stream — one's artist, the
    /// other's album — and display a track that does not exist.
    ///
    /// The year and the links are an exception to that last rule, and it is
    /// deliberate: they are **attached to the track, not to its playback**.
    /// An overriding contributor that brings no text (the real case of a
    /// plugin that only knows how to go fetch a listening link) is discarded
    /// by `text_block` — rightly so, it would otherwise erase the title from
    /// the ICY or the tags — but its answer did pass the door of `add`,
    /// which exempts it from the "entirely empty" refusal. Filling it only
    /// from the `fill_only`s threw it away silently. The declaration order
    /// remains the arbiter: the first to carry one wins, so the winner first
    /// if it has one.
    fn composed_text(&self) -> Track {
        let mut m = self.text_block();
        for plugin in &self.order {
            let Some(e) = self.enrichments.get(plugin) else { continue };
            // The year and the links are filled from **every** retained
            // enrichment, `fill_only` or not (see the doc above); the text,
            // for its part, is only filled from a `fill_only`.
            if m.year.is_none() && e.year.is_some() {
                m.year = e.year;
                m.provenance.fields.insert("year".into(), plugin.clone());
            }
            // Same rule as the other fields, decided with the owner: the
            // winner wins, a complement only fills an empty slot. No merging
            // per platform — that would be a policy invented for a case our
            // sources do not produce, none of them giving both YouTube and
            // Deezer.
            if m.links.is_empty() && !e.links.is_empty() {
                m.links = e.links.clone();
                m.provenance.fields.insert("links".into(), plugin.clone());
            }
            if !e.fill_only {
                continue;
            }
            if m.artist.is_none() && e.artist.is_some() {
                m.artist = e.artist.clone();
                m.provenance.fields.insert("artist".into(), plugin.clone());
            }
            if m.title.is_none() && e.title.is_some() {
                m.title = e.title.clone();
                m.provenance.fields.insert("title".into(), plugin.clone());
            }
            if m.album.is_none() && e.album.is_some() {
                m.album = e.album.clone();
                m.provenance.fields.insert("album".into(), plugin.clone());
            }
            if m.duration_s.is_none() && e.duration_s.is_some() {
                m.duration_s = e.duration_s;
                m.provenance.fields.insert("duration".into(), plugin.clone());
            }
        }
        // Those who searched without finding anything, in declaration order:
        // the same order as the arbitration, hence the same as the one read
        // everywhere else.
        m.provenance.misses = self
            .order
            .iter()
            .filter(|plugin| {
                self.enrichments.get(*plugin).is_some_and(|e| e.searched && brings_nothing(e))
            })
            .cloned()
            .collect();
        m
    }

    /// The retained contributor's text block: the first plugin that
    /// **overrides**, otherwise the file's tags, otherwise the raw ICY,
    /// otherwise nothing.
    ///
    /// The tags slot in between the two pre-existing layers, and that is
    /// their natural place: a `metadata` plugin goes far afield to fetch what
    /// the file does not say (an online database, a separate stream) and must
    /// therefore keep the upper hand; the ICY, for its part, describes a
    /// stream, not a file. In practice tags and ICY never coexist — the
    /// extraction returns `None` as soon as an `icy-*` key is present,
    /// precisely so that a station announcing its own name in `title` does
    /// not supplant the `icy-title` that carries the real track.
    ///
    /// The ICY is taken **as is** into `title`, without splitting on `" - "`:
    /// the convention exists but is not guaranteed, and a plugin enrichment
    /// provides already separated fields anyway. A station that only
    /// announces its own name or its jingles will therefore see that
    /// displayed — it is what it emits.
    fn text_block(&self) -> Track {
        for plugin in &self.order {
            if let Some(e) = self.enrichments.get(plugin) {
                // Two exclusions, and the second is not the first: a
                // `fill_only` is not a candidate by intention, an enrichment
                // **with no text at all** is not one by content. Since `add`
                // retains a cover alone (see its doc), an overriding plugin
                // can bring only an image — the real case of a Radio France
                // or OUI FM reading that carries `coverUrl` without a title.
                // Without this second exclusion, it would become the retained
                // block with all fields at `None` and would erase the title
                // the tags or the ICY were displaying: the cover gained would
                // cost the text.
                //
                // `is_empty()` and not "no title": it is exactly the
                // predicate the protocol already uses to say "this answer
                // says nothing about the text", cover and duration excluded.
                if e.fill_only || e.is_empty() {
                    continue;
                }
                // **The source first, the contributor next.** An enrichment
                // that declares `derived_from` only re-reads what another
                // announced (the splitting of an ICY): attributing the
                // information to it would erase the one that really holds it.
                // `origin` follows the same rule, otherwise the two halves of
                // the same frame would contradict each other.
                let source = e.derived_from.as_deref().unwrap_or(plugin);
                let mut m = Track {
                    artist: e.artist.clone(),
                    title: e.title.clone(),
                    album: e.album.clone(),
                    duration_s: e.duration_s,
                    year: e.year,
                    links: e.links.clone(),
                    origin: Some(source.to_string()),
                    ..Default::default()
                };
                note_fields(&mut m, source, e.derived_from.as_deref().map(|_| plugin.as_str()));
                return m;
            }
        }
        if let Some(tags) = &self.tags {
            let mut m = tags.clone();
            // The tags already carry `origin`, but not the per-field
            // provenance: this is where we set it, on what they actually
            // provide.
            note_fields(&mut m, ORIGIN_TAGS, None);
            return m;
        }
        match &self.icy {
            Some(icy) => {
                let mut m = Track {
                    title: Some(icy.clone()),
                    origin: Some(ORIGIN_ICY.to_string()),
                    ..Default::default()
                };
                note_fields(&mut m, ORIGIN_ICY, None);
                m
            }
            None => Track::default(),
        }
    }

    /// Position declared by the **winner** of the arbitration, if it declares
    /// one.
    ///
    /// Ignores the `fill_only`s, exactly like `winner()`: a position only
    /// makes sense coming from whoever actually follows the stream's
    /// progress, never from a complement that only fills a text or a cover.
    /// `Core::handle_enrichment` only calls this method after checking
    /// `winner() == Some(plugin)` to decide whether to re-anchor: the two
    /// methods must name the same winner, otherwise the safeguard would fire
    /// to anchor on the value of a contributor different from the one it just
    /// identified — or on `None` if that contributor, declared before the
    /// winner in `plugins.toml`, does not itself declare a position.
    ///
    /// Output separate from `state()` rather than slipped into `Track`:
    /// `Track` describes what is displayable of a track, values stable while
    /// it plays, whereas a position is only worth the instant it was said.
    /// This module has no clock anyway, and that is deliberate (see the
    /// header): it is up to the core to anchor that value and advance it.
    pub fn position_s(&self) -> Option<u32> {
        for plugin in &self.order {
            if let Some(e) = self.enrichments.get(plugin) {
                if e.fill_only {
                    continue;
                }
                return e.position_s;
            }
        }
        None
    }

    /// Duration declared by the **winner** of the arbitration, if it declares
    /// one, complemented by a `fill_only` if it does not.
    ///
    /// Deliberately **asymmetric** with `position_s()` just above, and it is
    /// not an oversight: this method lets a `fill_only` fill a duration the
    /// winner does not declare, whereas `winner()` ignores them outright.
    /// `state()` does the same (a file whose tags do not carry the duration,
    /// complemented by a plugin that knows it) and `known()` republishes that
    /// composed value: an accessor that ignored the `fill_only`s would cap
    /// the position (`Core::refresh_position`) against a duration different
    /// from the one displayed on screen. A position, for its part, has no
    /// such equivalent — nobody "complements" a progress — hence the
    /// asymmetry with `position_s()`.
    ///
    /// It therefore does **not quite** compose like `state()`: only the
    /// enrichments are consulted, never `self.tags`. An earlier version of
    /// this comment promised the opposite. Inert today, and it must be said
    /// so the next reading does not stop there: `player::mpv::file_tags`
    /// hardcodes `duration_s: None` — mpv does not report the duration in its
    /// `metadata` property, it comes from its own `Progress` — so the tags
    /// layer never has a duration to bring and the divergence has nothing to
    /// bite on. The day it carries one, this is where it would have to be
    /// added, between the winner and the `fill_only`s, at the exact place the
    /// tags occupy in `text_block`.
    ///
    /// Same reason for being as `position_s` just above for the output
    /// separate from `state()`: reading an integer must not cost the
    /// reconstruction of a whole `Track`, cloned strings included — which the
    /// position capping would do once per second for the entire playback of a
    /// stream.
    pub fn duration_s(&self) -> Option<u32> {
        let mut duration = None;
        for plugin in &self.order {
            if let Some(e) = self.enrichments.get(plugin)
                && !e.fill_only
            {
                duration = e.duration_s;
                break;
            }
        }
        if duration.is_none() {
            for plugin in &self.order {
                if let Some(e) = self.enrichments.get(plugin)
                    && e.fill_only && e.duration_s.is_some()
                {
                    duration = e.duration_s;
                    break;
                }
            }
        }
        duration
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn enrichment(identity: Value, artist: &str, title: &str) -> Enrichment {
        Enrichment {
            identity,
            artist: Some(artist.into()),
            title: Some(title.into()),
            ..Default::default()
        }
    }

    /// Factory: an overriding enrichment, with the given fields.
    fn overriding(id: &Value, artist: Option<&str>, album: Option<&str>) -> Enrichment {
        Enrichment {
            identity: id.clone(),
            artist: artist.map(str::to_string),
            title: Some("T".into()),
            album: album.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn an_overriding_contributor_provides_its_block_and_the_fill_only_fills() {
        let id = json!({"kind": "stream"});
        let mut m = Metadata::new(vec!["specific".into(), "generic".into()]);
        m.set_identity(Some(id.clone()));
        // The specific one knows the artist, not the album.
        assert!(m.add("specific", overriding(&id, Some("A"), None)));
        // The generic one complements: it does not replace the artist, it
        // fills in the album that was missing.
        assert!(m.add(
            "generic",
            Enrichment {
                identity: id.clone(),
                artist: Some("NOT HIM".into()),
                album: Some("ALBUM".into()),
                fill_only: true,
                ..Default::default()
            }
        ));
        let state = m.state();
        assert_eq!(state.artist.as_deref(), Some("A"), "a fill_only never replaces");
        assert_eq!(state.album.as_deref(), Some("ALBUM"), "a fill_only fills a gap");
        assert_eq!(state.origin.as_deref(), Some("specific"));
    }

    #[test]
    fn two_overriding_contributors_are_not_mixed() {
        // Composing field by field between two overriding ones would mix two
        // readings of the same stream and display a track that does not exist.
        let id = json!({"kind": "stream"});
        let mut m = Metadata::new(vec!["first".into(), "second".into()]);
        m.set_identity(Some(id.clone()));
        m.add("first", overriding(&id, Some("A"), None));
        m.add("second", overriding(&id, Some("B"), Some("SECOND'S ALBUM")));
        let state = m.state();
        assert_eq!(state.artist.as_deref(), Some("A"));
        assert_eq!(state.album, None, "the first one's block is authoritative, gaps included");
    }

    #[test]
    fn the_cover_follows_the_tiers_source_then_tags_then_plugin() {
        let id = json!({"kind": "file", "path": "/mnt/nas/a.flac"});
        let mut m = Metadata::new(vec!["musicbrainz".into()]);
        m.set_identity(Some(id.clone()));

        // The plugin alone: it is the one retained.
        assert!(m.add(
            "musicbrainz",
            Enrichment {
                identity: id.clone(),
                title: Some("T".into()),
                cover: Some(CoverRef::Url { url: "https://coverartarchive.org/x/front-500".into() }),
                fill_only: true,
                ..Default::default()
            }
        ));
        let (_, origin) = m.selected_cover().expect("the plugin provides a cover");
        assert_eq!(origin, "musicbrainz");

        // The embedded cover, read by the core, goes ahead of the plugin.
        assert!(m.set_cover_tags(Some(CoverRef::Path { path: "/tmp/embedded.jpg".into() })));
        assert_eq!(m.selected_cover().unwrap().1, ORIGIN_TAGS);

        // The file placed alongside, declared by the Source, goes ahead of
        // everything.
        assert!(m.set_cover_source(
            Some(CoverRef::Path { path: "/mnt/nas/Album/folder.jpg".into() }),
            "files"
        ));
        let (r, origin) = m.selected_cover().unwrap();
        assert_eq!(origin, "files");
        assert_eq!(r, CoverRef::Path { path: "/mnt/nas/Album/folder.jpg".into() });
    }

    #[test]
    fn a_cover_alone_is_retained_and_names_its_contributor() {
        // The central defect of the layer: `Enrichment::is_empty` deliberately
        // ignores `cover` — so that a cover alone does not win the
        // arbitration of the *text* — and `add` refused any empty enrichment,
        // a refusal predating covers. `musicbrainz`'s generic relay emits
        // precisely a cover and nothing else: it was therefore refused at the
        // door, and the Cover Art Archive path — the very reason for the
        // protocol's re-tiering — never contributed anything. All the other
        // cover tests here give a `title` to their enrichment, and that is why
        // none of them saw it.
        let id = json!({"kind": "file", "path": "/mnt/nas/a.flac"});
        let mut m = Metadata::new(vec!["musicbrainz".into()]);
        m.set_identity(Some(id.clone()));
        assert!(m.add(
            "musicbrainz",
            Enrichment {
                identity: id,
                cover: Some(CoverRef::Url {
                    url: "https://coverartarchive.org/release/x/front-500".into()
                }),
                fill_only: true,
                ..Default::default()
            }
        ));
        let (r, origin) = m.selected_cover().expect("a cover alone must be retained");
        assert_eq!(origin, "musicbrainz");
        assert_eq!(r, CoverRef::Url { url: "https://coverartarchive.org/release/x/front-500".into() });
        assert!(m.known().cover);
        // And nothing of the text: the `is_empty` convention has not moved.
        assert!(m.state().is_empty(), "a cover alone brings no text");
    }

    #[test]
    fn an_overriding_cover_alone_does_not_clear_the_title_already_displayed() {
        // The trap of half 2, and it is real: `radiofrance-metas` and
        // `ouifm-metas` override (`fill_only` false) and build their frame
        // from a reading that can carry `coverUrl` without a title. Retained
        // for its cover — it has to be, half 1 — such an enrichment must not
        // for all that become the retained text block with all its fields at
        // `None`: that would trade the displayed line for an image.
        let mut m = Metadata::new(vec!["radiofrance".into()]);
        let id = json!({"kind": "stream", "url": "https://fip"});
        m.set_identity(Some(id.clone()));
        m.set_icy("Miles Davis - So What".into());
        assert!(m.add(
            "radiofrance",
            Enrichment {
                identity: id.clone(),
                cover: Some(CoverRef::Url { url: "https://www.radiofrance.fr/x.jpg".into() }),
                ..Default::default()
            }
        ));
        let state = m.state();
        assert_eq!(state.title.as_deref(), Some("Miles Davis - So What"), "the ICY must stay displayed");
        assert_eq!(state.origin.as_deref(), Some("icy"));
        assert_eq!(m.selected_cover().unwrap().1, "radiofrance", "and the cover is indeed its own");

        // Same guarantee against the file's tags, the other layer this empty
        // block would have covered up.
        let mut m = Metadata::new(vec!["radiofrance".into()]);
        m.set_identity(Some(id.clone()));
        m.set_tags(Track {
            title: Some("So What".into()),
            origin: Some(ORIGIN_TAGS.to_string()),
            ..Default::default()
        });
        assert!(m.add(
            "radiofrance",
            Enrichment {
                identity: id,
                cover: Some(CoverRef::Url { url: "https://www.radiofrance.fr/x.jpg".into() }),
                ..Default::default()
            }
        ));
        assert_eq!(m.state().title.as_deref(), Some("So What"));
        assert_eq!(m.state().origin.as_deref(), Some(ORIGIN_TAGS));
    }

    #[test]
    fn a_fill_only_declared_first_does_not_beat_a_specialized_plugin_for_the_cover() {
        // The mechanism the brief justifies explicitly: two passes, not one,
        // otherwise a `fill_only` declared high in `plugins.toml` would go
        // ahead of a specialized plugin declared lower, the opposite of its
        // intention. Collapsing the two passes into a single loop makes this
        // test fail while the others say nothing about it.
        let id = json!({"kind": "file", "path": "/a.flac"});
        let mut m = Metadata::new(vec!["filler".into(), "specialized".into()]);
        m.set_identity(Some(id.clone()));
        assert!(m.add(
            "filler",
            Enrichment {
                identity: id.clone(),
                title: Some("T".into()),
                cover: Some(CoverRef::Url { url: "https://coverartarchive.org/a/front-500".into() }),
                fill_only: true,
                ..Default::default()
            }
        ));
        assert!(m.add(
            "specialized",
            Enrichment {
                identity: id,
                title: Some("T".into()),
                cover: Some(CoverRef::Url { url: "https://coverartarchive.org/b/front-500".into() }),
                ..Default::default()
            }
        ));
        let (r, origin) = m.selected_cover().expect("the specialized plugin provides a cover");
        assert_eq!(origin, "specialized", "declared lower, it must nevertheless not yield to the fill_only");
        assert_eq!(r, CoverRef::Url { url: "https://coverartarchive.org/b/front-500".into() });
    }

    #[test]
    fn a_known_carrying_only_the_raw_icy_is_impossible() {
        // `Known::is_empty` does not count `stream_title`, and I first
        // believed it an oversight: this predicate is the
        // `skip_serializing_if` of `NowPlaying::known`, so a `Known` judged
        // empty **disappears from the frame**, and a plugin would never see
        // the raw ICY string again.
        //
        // It is not a defect: the state is unreachable. As soon as `icy` is
        // filled in, `text_block` guarantees a title — a winner's, the tags',
        // or the ICY itself as a last resort — so `is_empty` is false through
        // another field. And `set_icy` never receives a blank string,
        // `player::mpv::icy_title` filtering it upstream.
        //
        // But this safety rests on an invariant held **elsewhere**, not on
        // the predicate. This test locks it in: if `text_block` one day
        // stopped carrying the ICY into the title, the omission would become
        // a silent loss, and this is where we would learn it.
        let id = json!({"kind": "stream"});
        let mut m = Metadata::new(vec!["plugin".into()]);
        m.set_identity(Some(id));
        assert!(m.set_icy("Mandrillus Sphynx - Bikwix".into()));
        let k = m.known();
        assert_eq!(k.stream_title.as_deref(), Some("Mandrillus Sphynx - Bikwix"));
        assert!(!k.is_empty(), "the ICY alone already fills the title, so the frame carries it");
        assert_eq!(k.title.as_deref(), Some("Mandrillus Sphynx - Bikwix"), "the invariant in question");
    }

    #[test]
    fn the_year_and_links_follow_the_winner_rule() {
        // Rule settled with the owner: the winner wins, a `fill_only` only
        // fills an empty slot. No merging per platform — that would be a
        // policy invented for a case our sources do not produce.
        use ritornello_proto::Link;
        let id = json!({"kind": "stream"});
        let mut m = Metadata::new(vec!["winner".into(), "filler".into()]);
        m.set_identity(Some(id.clone()));
        m.add(
            "winner",
            Enrichment {
                identity: id.clone(),
                title: Some("T".into()),
                year: Some(1959),
                links: vec![Link::Youtube { url: "https://www.youtube.com/watch?v=a".into() }],
                ..Default::default()
            },
        );
        m.add(
            "filler",
            Enrichment {
                identity: id.clone(),
                year: Some(1999),
                links: vec![Link::Deezer { url: "https://www.deezer.com/track/1".into() }],
                fill_only: true,
                ..Default::default()
            },
        );
        let state = m.state();
        assert_eq!(state.year, Some(1959), "the fill_only does not overwrite");
        assert_eq!(
            state.links,
            vec![Link::Youtube { url: "https://www.youtube.com/watch?v=a".into() }],
            "no merging: the winner's links, and them alone"
        );
    }

    #[test]
    fn a_fill_only_fills_the_year_and_links_the_winner_does_not_know() {
        use ritornello_proto::Link;
        let id = json!({"kind": "stream"});
        let mut m = Metadata::new(vec!["winner".into(), "filler".into()]);
        m.set_identity(Some(id.clone()));
        m.add(
            "winner",
            Enrichment { identity: id.clone(), title: Some("T".into()), ..Default::default() },
        );
        m.add(
            "filler",
            Enrichment {
                identity: id.clone(),
                year: Some(1999),
                links: vec![Link::Deezer { url: "https://www.deezer.com/track/1".into() }],
                fill_only: true,
                ..Default::default()
            },
        );
        let state = m.state();
        assert_eq!(state.year, Some(1999));
        assert_eq!(state.links.len(), 1);
        // And `known()` republishes the composed year, so that a plugin knows
        // it is already held.
        assert_eq!(m.known().year, Some(1999));
    }

    #[test]
    fn a_contributor_without_text_still_brings_its_year_and_links() {
        // The mirror of the previous test, on the **overriding** side. A
        // contributor that only brings a year and/or links is accepted by
        // `add` (exempted from the "entirely empty" refusal), then
        // `text_block` discards it — rightly so: without text it cannot be
        // the retained block, otherwise it would erase the title from the ICY
        // or the tags. Still, it was then lost entirely: `composed_text` only
        // filled the year and the links from the `fill_only`s. Its answer was
        // therefore thrown away silently when it had passed the door.
        use ritornello_proto::Link;
        let id = json!({"kind": "stream"});
        let mut m = Metadata::new(vec!["plugin".into()]);
        m.set_identity(Some(id.clone()));
        assert!(m.set_icy("Mandrillus Sphynx - Bikwix".into()));
        assert!(m.add(
            "plugin",
            Enrichment {
                identity: id,
                year: Some(1959),
                links: vec![Link::Youtube { url: "https://www.youtube.com/watch?v=a".into() }],
                ..Default::default()
            }
        ));
        let state = m.state();
        assert_eq!(state.year, Some(1959), "the textless contributor's year is retained");
        assert_eq!(
            state.links,
            vec![Link::Youtube { url: "https://www.youtube.com/watch?v=a".into() }],
            "its links too"
        );
        // And the title remains the ICY's: filling must not promote this
        // contributor to retained block.
        assert_eq!(state.title.as_deref(), Some("Mandrillus Sphynx - Bikwix"));
        assert_eq!(state.origin.as_deref(), Some(ORIGIN_ICY));
    }

    #[test]
    fn a_second_overriding_contributor_does_not_replace_the_winner_year() {
        // The check on the previous test: filling from **every** retained
        // enrichment must not degenerate into "the last one wins". The
        // declaration order remains the arbiter.
        use ritornello_proto::Link;
        let id = json!({"kind": "stream"});
        let mut m = Metadata::new(vec!["winner".into(), "next".into()]);
        m.set_identity(Some(id.clone()));
        m.add(
            "winner",
            Enrichment {
                identity: id.clone(),
                title: Some("T".into()),
                year: Some(1959),
                links: vec![Link::Youtube { url: "https://www.youtube.com/watch?v=a".into() }],
                ..Default::default()
            },
        );
        m.add(
            "next",
            Enrichment {
                identity: id,
                title: Some("T2".into()),
                year: Some(2017),
                links: vec![Link::Deezer { url: "https://www.deezer.com/track/1".into() }],
                ..Default::default()
            },
        );
        let state = m.state();
        assert_eq!(state.year, Some(1959), "the winner's year remains the winner's");
        assert_eq!(
            state.links,
            vec![Link::Youtube { url: "https://www.youtube.com/watch?v=a".into() }],
            "the winner's links, and them alone"
        );
    }

    #[test]
    fn the_winner_ignores_a_fill_only_that_arrived_first() {
        let id = json!({"kind": "stream"});
        let mut m = Metadata::new(vec!["filler".into(), "specialized".into()]);
        m.set_identity(Some(id.clone()));
        assert!(m.add(
            "filler",
            Enrichment { identity: id.clone(), title: Some("T".into()), fill_only: true, ..Default::default() }
        ));
        assert_eq!(m.winner(), None, "a fill_only alone is never the winner");
        assert!(m.add(
            "specialized",
            Enrichment { identity: id, title: Some("T2".into()), ..Default::default() }
        ));
        assert_eq!(m.winner(), Some("specialized"));
    }

    #[test]
    fn an_overriding_plugin_invalidates_the_key_of_an_already_published_fill_only_cover() {
        // Sequence the first version missed: a fill_only provides a cover,
        // the core goes and fetches the bytes and publishes the key; then a
        // specialized plugin answers with a different cover. `add` is the
        // third mutation path of the retained reference, on a par with
        // `set_cover_source`/`set_cover_tags`, and must therefore invalidate
        // the key exactly as they already do.
        let id = json!({"kind": "file", "path": "/a.flac"});
        let mut m = Metadata::new(vec!["specialized".into(), "musicbrainz".into()]);
        m.set_identity(Some(id.clone()));
        assert!(m.add(
            "musicbrainz",
            Enrichment {
                identity: id.clone(),
                title: Some("T".into()),
                cover: Some(CoverRef::Url { url: "https://coverartarchive.org/a/front-500".into() }),
                fill_only: true,
                ..Default::default()
            }
        ));
        assert_eq!(m.selected_cover().unwrap().1, "musicbrainz");
        m.set_cover_href(Some("keya".into()));
        assert_eq!(m.state().cover_href.as_deref(), Some("/api/cover/keya"));

        assert!(m.add(
            "specialized",
            Enrichment {
                identity: id,
                title: Some("T".into()),
                cover: Some(CoverRef::Url { url: "https://coverartarchive.org/b/front-500".into() }),
                ..Default::default()
            }
        ));
        let (r, origin) = m.selected_cover().unwrap();
        assert_eq!(origin, "specialized");
        assert_eq!(r, CoverRef::Url { url: "https://coverartarchive.org/b/front-500".into() });
        assert!(
            m.state().cover_href.is_none(),
            "the stale key must not stay published under the new origin"
        );
    }

    #[test]
    fn an_identity_change_clears_the_cover_like_the_rest() {
        let id = json!({"kind": "file", "path": "/a.flac"});
        let mut m = Metadata::new(vec![]);
        m.set_identity(Some(id));
        m.set_cover_source(Some(CoverRef::Path { path: "/a/folder.jpg".into() }), "files");
        m.set_cover_tags(Some(CoverRef::Path { path: "/b/embedded.jpg".into() }));
        m.set_cover_href(Some("abcd".into()));
        assert!(m.set_identity(Some(json!({"kind": "file", "path": "/b.flac"}))));
        assert!(m.selected_cover().is_none(), "leaving the previous cover would be more misleading than nothing");
        assert!(m.state().cover_href.is_none());
    }

    #[test]
    fn a_cover_whose_fetch_failed_yields_to_the_next_one() {
        // The design anticipates it: "a pattern that breaks yields a silence".
        // Without a memory of the failure, that silence was final —
        // `known.cover` stayed true because a reference was *retained*, so
        // `musicbrainz` kept quiet, and it would have been outranked anyway
        // had it spoken.
        let id = json!({"kind": "stream", "url": "https://fip"});
        let mut m = Metadata::new(vec!["radiofrance".into(), "musicbrainz".into()]);
        m.set_identity(Some(id.clone()));
        let dead = CoverRef::Url { url: "https://api.radiofrance.fr/v1/embed/image/rusty".into() };
        assert!(m.add(
            "radiofrance",
            Enrichment {
                identity: id.clone(),
                artist: Some("Miles Davis".into()),
                title: Some("So What".into()),
                cover: Some(dead.clone()),
                ..Default::default()
            }
        ));
        assert!(m.known().cover, "as long as we do not know, the reference is held");
        // A key that is not its own changes nothing.
        assert!(!m.mark_cover_failed("another-key".into()));
        assert!(m.known().cover);

        m.set_cover_href(Some(crate::cover::key(&dead)));
        assert!(m.mark_cover_failed(crate::cover::key(&dead)));
        assert!(!m.known().cover, "an unkept promise must no longer silence the others");
        assert!(m.selected_cover().is_none());
        assert!(m.state().cover_href.is_none(), "the published key no longer describes anything");
        // The text, for its part, has not moved: it is the cover that failed.
        assert_eq!(m.state().title.as_deref(), Some("So What"));

        // Which finally lets `musicbrainz` compensate.
        let caa = CoverRef::Url { url: "https://coverartarchive.org/release/x/front-500".into() };
        assert!(m.add(
            "musicbrainz",
            Enrichment {
                identity: id,
                cover: Some(caa.clone()),
                fill_only: true,
                ..Default::default()
            }
        ));
        let (r, origin) = m.selected_cover().expect("the compensator must get through");
        assert_eq!((r, origin.as_str()), (caa, "musicbrainz"));
    }

    #[test]
    fn an_identity_change_forgets_cover_failures() {
        // A failure holds for a reference **of that track**: the same URL may
        // answer for the next one (a CDN that woke up), and a list surviving
        // the identity would prevent asking for it again.
        let mut m = Metadata::new(vec!["radiofrance".into()]);
        m.set_identity(Some(json!({"url": "one"})));
        let r = CoverRef::Url { url: "https://www.radiofrance.fr/x.jpg".into() };
        m.set_cover_source(Some(r.clone()), "radio");
        assert!(m.mark_cover_failed(crate::cover::key(&r)));
        assert!(m.selected_cover().is_none());

        assert!(m.set_identity(Some(json!({"url": "two"}))));
        m.set_cover_source(Some(r.clone()), "radio");
        assert_eq!(
            m.selected_cover().map(|(r, _)| r),
            Some(r),
            "the failure slate must be wiped clean with the rest"
        );
    }

    #[test]
    fn known_exposes_what_is_known_and_whether_a_cover_is_held() {
        let id = json!({"kind": "stream"});
        let mut m = Metadata::new(vec!["p".into()]);
        m.set_identity(Some(id.clone()));
        m.add("p", overriding(&id, Some("A"), None));
        let k = m.known();
        assert_eq!(k.artist.as_deref(), Some("A"));
        assert_eq!(k.album, None, "an empty field is what invites a contributor to search");
        assert!(!k.cover);

        m.set_cover_tags(Some(CoverRef::Path { path: "/x/c.jpg".into() }));
        assert!(m.known().cover, "a held cover must silence a fill_only");
    }

    #[test]
    fn the_published_cover_href_is_the_local_url() {
        let id = json!({"kind": "file", "path": "/a.flac"});
        let mut m = Metadata::new(vec![]);
        m.set_identity(Some(id));
        m.set_cover_source(Some(CoverRef::Path { path: "/a/folder.jpg".into() }), "files");
        // As long as the bytes are not in hand, nothing is published: the UI
        // must never receive the URL of a broken image.
        assert!(m.state().cover_href.is_none());
        m.set_cover_href(Some("1a2b3c4d".into()));
        let state = m.state();
        assert_eq!(state.cover_href.as_deref(), Some("/api/cover/1a2b3c4d"));
        assert_eq!(state.cover_origin.as_deref(), Some("files"));

        // The key may become None again (fetch invalidated, not redone yet)
        // while the reference itself stays retained: nothing must be
        // displayed as long as no valid key is published.
        m.set_cover_href(None);
        assert!(m.state().cover_href.is_none(), "key erased, the reference nevertheless still retained");
        assert!(m.selected_cover().is_some(), "the reference itself has not moved");
    }

    #[test]
    fn a_stale_enrichment_is_ignored() {
        let mut m = Metadata::new(vec!["ouifm".into()]);
        m.set_identity(Some(json!({"url": "two"})));
        let retained = m.add("ouifm", enrichment(json!({"url": "one"}), "A", "T"));
        assert!(!retained);
        assert!(m.state().is_empty());
    }

    #[test]
    fn an_identity_change_clears_the_icy_and_the_enrichments() {
        let mut m = Metadata::new(vec!["ouifm".into()]);
        m.set_identity(Some(json!({"url": "one"})));
        assert!(m.set_icy("Station - Jingle".into()));
        assert!(m.add("ouifm", enrichment(json!({"url": "one"}), "A", "T")));
        assert!(!m.state().is_empty());

        assert!(m.set_identity(Some(json!({"url": "two"}))));
        assert!(m.state().is_empty(), "the slate must be wiped clean immediately");
    }

    #[test]
    fn an_unchanged_identity_resets_nothing() {
        // A Source may give the same identity again (stream restart after a
        // cutoff): it is not a new track, the display must hold.
        let mut m = Metadata::new(vec!["ouifm".into()]);
        m.set_identity(Some(json!({"url": "one"})));
        m.set_icy("Miles Davis - So What".into());
        assert!(!m.set_identity(Some(json!({"url": "one"}))));
        assert_eq!(m.state().title.as_deref(), Some("Miles Davis - So What"));
    }

    #[test]
    fn a_plugin_wins_over_the_file_tags() {
        // Same rule as against the ICY, and for the same reason: a plugin
        // goes far afield to fetch what the file does not say, and what it
        // learned must stay displayed.
        let mut m = Metadata::new(vec!["musicbrainz".into()]);
        m.set_identity(Some(json!({"kind": "file", "path": "/x/03.flac"})));
        m.set_tags(Track {
            title: Some("track 03".into()),
            origin: Some(ORIGIN_TAGS.to_string()),
            ..Default::default()
        });
        m.add(
            "musicbrainz",
            enrichment(json!({"kind": "file", "path": "/x/03.flac"}), "Miles Davis", "So What"),
        );
        let state = m.state();
        assert_eq!(state.title.as_deref(), Some("So What"));
        assert_eq!(state.origin.as_deref(), Some("musicbrainz"));
    }

    #[test]
    fn tags_win_over_the_icy_and_are_attributed() {
        // The two never coexist in practice (the extraction goes quiet as
        // soon as an ICY key is there), but the order must be written down:
        // the ICY describes a stream, the tags describe the file actually
        // played.
        let mut m = Metadata::new(vec![]);
        m.set_icy("Station - Jingle".into());
        m.set_tags(Track {
            artist: Some("Miles Davis".into()),
            title: Some("So What".into()),
            origin: Some(ORIGIN_TAGS.to_string()),
            ..Default::default()
        });
        let state = m.state();
        assert_eq!(state.title.as_deref(), Some("So What"));
        assert_eq!(state.origin.as_deref(), Some("tags"));
    }

    #[test]
    fn an_identity_change_also_clears_the_tags() {
        // Without this, the previous track's tags would stay displayed until
        // mpv publishes the next one's.
        let mut m = Metadata::new(vec![]);
        m.set_identity(Some(json!({"kind": "file", "path": "/x/01.mp3"})));
        m.set_tags(Track { title: Some("Track 1".into()), ..Default::default() });
        assert!(m.set_identity(Some(json!({"kind": "file", "path": "/x/02.mp3"}))));
        assert!(m.state().is_empty());
    }

    #[test]
    fn repeated_tags_trigger_nothing() {
        // mpv republishes `metadata` more often than it changes: without this
        // deduplication, each republication would repaint the displays.
        let mut m = Metadata::new(vec![]);
        let tags = Track { title: Some("So What".into()), ..Default::default() };
        assert!(m.set_tags(tags.clone()));
        assert!(!m.set_tags(tags));
    }

    #[test]
    fn the_enrichment_wins_over_the_icy() {
        // Measured case from OUI FM: its ICY header is the filler text "Now
        // Playing info goes here", which the plugin's override must overwrite
        // — otherwise that text would be displayed in place of the track.
        let mut m = Metadata::new(vec!["ouifm".into()]);
        m.set_identity(Some(json!({"url": "one"})));
        m.set_icy("Now Playing info goes here".into());
        m.add("ouifm", enrichment(json!({"url": "one"}), "Shaka Ponk", "Wanna Get Free"));
        let state = m.state();
        assert_eq!(state.artist.as_deref(), Some("Shaka Ponk"));
        assert_eq!(state.title.as_deref(), Some("Wanna Get Free"));
        assert_eq!(state.origin.as_deref(), Some("ouifm"));
    }

    #[test]
    fn the_icy_alone_is_displayed_raw_and_attributed() {
        let mut m = Metadata::new(vec![]);
        m.set_identity(Some(json!({"url": "one"})));
        m.set_icy("Mandrillus Sphynx - Bikwix".into());
        let state = m.state();
        // No splitting on " - ": the convention is not guaranteed.
        assert_eq!(state.title.as_deref(), Some("Mandrillus Sphynx - Bikwix"));
        assert_eq!(state.artist, None);
        assert_eq!(state.origin.as_deref(), Some("icy"));
    }

    #[test]
    fn the_first_declared_plugin_wins_whatever_the_arrival_order() {
        // The second declared one answers **first**: this is the case that
        // distinguishes "declaration order" from "first come". The result
        // must not depend on network latency, otherwise the same installation
        // would display something different from one startup to the next.
        let mut m = Metadata::new(vec!["priority".into(), "secondary".into()]);
        let id = json!({"url": "one"});
        m.set_identity(Some(id.clone()));
        assert!(m.add("secondary", enrichment(id.clone(), "Second", "Second title")));
        assert_eq!(m.state().artist.as_deref(), Some("Second"));

        assert!(m.add("priority", enrichment(id.clone(), "First", "First title")));
        assert_eq!(m.state().artist.as_deref(), Some("First"));

        // And a new enrichment from the lower-priority one does not take
        // over again.
        assert!(m.add("secondary", enrichment(id, "Second again", "Second title again")));
        assert_eq!(m.state().artist.as_deref(), Some("First"));
    }

    #[test]
    fn an_empty_enrichment_lets_the_next_one_win() {
        let mut m = Metadata::new(vec!["priority".into(), "secondary".into()]);
        let id = json!({"url": "one"});
        m.set_identity(Some(id.clone()));
        let empty = Enrichment { identity: id.clone(), ..Default::default() };
        assert!(!m.add("priority", empty), "an empty enrichment counts as a non-answer");
        assert!(m.add("secondary", enrichment(id, "Second", "Title")));
        assert_eq!(m.state().artist.as_deref(), Some("Second"));
    }

    #[test]
    fn an_enrichment_outside_playback_is_ignored() {
        let mut m = Metadata::new(vec!["ouifm".into()]);
        // Nothing playing anymore: nothing to enrich.
        assert!(!m.add("ouifm", enrichment(json!({"url": "one"}), "A", "T")));
        assert!(m.state().is_empty());
    }

    #[test]
    fn an_undeclared_plugin_is_refused() {
        // Without this refusal, a plugin absent from `plugins.toml` would have
        // no defined priority and would never appear in the resolution: its
        // enrichment would be stored for nothing, which would give a silently
        // inert state rather than a warning.
        let mut m = Metadata::new(vec!["ouifm".into()]);
        let id = json!({"url": "one"});
        m.set_identity(Some(id.clone()));
        assert!(!m.add("unknown", enrichment(id, "A", "T")));
        assert!(m.state().is_empty());
    }

    #[test]
    fn the_provenance_names_each_field_contributor() {
        // **The question nothing answered**: "why is this title wrong?".
        // `origin` only names the winner of the text block, when the screen
        // is composed by several hands — a `fill_only` fills, the year and
        // the links are taken from everywhere, the cover often comes from
        // elsewhere.
        let mut m = Metadata::new(vec!["winner".into(), "complement".into()]);
        m.set_identity(Some(json!(1)));
        m.add(
            "winner",
            Enrichment {
                identity: json!(1),
                artist: Some("Miles Davis".into()),
                title: Some("So What".into()),
                ..Default::default()
            },
        );
        m.add(
            "complement",
            Enrichment {
                identity: json!(1),
                album: Some("Kind of Blue".into()),
                year: Some(1959),
                fill_only: true,
                ..Default::default()
            },
        );

        let fields = m.state().provenance.fields;
        assert_eq!(fields.get("artist").map(String::as_str), Some("winner"));
        assert_eq!(fields.get("title").map(String::as_str), Some("winner"));
        // Filled by the other: this is precisely what `origin` could not say,
        // since it names "winner" for the whole block.
        assert_eq!(fields.get("album").map(String::as_str), Some("complement"));
        assert_eq!(fields.get("year").map(String::as_str), Some("complement"));
        assert_eq!(m.state().origin.as_deref(), Some("winner"));
        // Nothing is named for an absent field: the map says where what you
        // see comes from, not who was consulted.
        assert!(!fields.contains_key("duration"), "no duration was provided");
    }

    #[test]
    fn a_contributor_rereading_a_source_does_not_become_the_source() {
        // **The defect reported by the owner**, on a radio without a metadata
        // plugin: the ICY gave the information, `musicbrainz` split it, and
        // the screen displayed "Title: musicbrainz". Yet it had taught nobody
        // anything — the station remains the source, it only read it
        // differently.
        let mut m = Metadata::new(vec!["musicbrainz".into()]);
        m.set_identity(Some(json!(1)));
        m.set_icy("Miles Davis - So What".into());
        assert!(m.add(
            "musicbrainz",
            Enrichment {
                identity: json!(1),
                artist: Some("Miles Davis".into()),
                title: Some("So What".into()),
                derived_from: Some(ORIGIN_ICY.to_string()),
                ..Default::default()
            }
        ));

        let state = m.state();
        // The values are indeed the contributor's — it did the splitting, and
        // it is its split that is displayed.
        assert_eq!(state.artist.as_deref(), Some("Miles Davis"));
        // But the **source** is the station, in both halves of the frame.
        assert_eq!(state.origin.as_deref(), Some(ORIGIN_ICY));
        assert_eq!(state.provenance.fields.get("title").map(String::as_str), Some(ORIGIN_ICY));
        assert_eq!(state.provenance.fields.get("artist").map(String::as_str), Some(ORIGIN_ICY));
        // And the rework is noted **alongside**, without erasing anything:
        // both facts hold together.
        assert_eq!(state.provenance.derived.get("title").map(String::as_str), Some("musicbrainz"));
        assert_eq!(state.provenance.derived.get("artist").map(String::as_str), Some("musicbrainz"));
    }

    #[test]
    fn an_enrichment_that_says_nothing_withdraws_the_previous_one() {
        // **The other half of the defect just above.** The plugin signed the
        // station's string with its own name for one reason only: emitting
        // nothing left its *previous* enrichment in place — `set_icy` does not
        // erase them, by an owner decision — and a radio's identity does not
        // change from one track to the next, so the previous track's title
        // stayed on screen. Withdrawing makes "I have nothing to say about
        // this track" expressible, and it is what lets the plugin stop
        // claiming a rework it did not do.
        let mut m = Metadata::new(vec!["musicbrainz".into()]);
        m.set_identity(Some(json!(1)));
        m.set_icy("Miles Davis - So What".into());
        assert!(m.add(
            "musicbrainz",
            Enrichment {
                identity: json!(1),
                artist: Some("Miles Davis".into()),
                title: Some("So What".into()),
                derived_from: Some(ORIGIN_ICY.to_string()),
                ..Default::default()
            }
        ));
        assert_eq!(m.state().artist.as_deref(), Some("Miles Davis"));

        // Next title on the same station, and this one says nothing splittable.
        m.set_icy("Vous ecoutez Radio X".into());
        assert!(
            m.add("musicbrainz", Enrichment { identity: json!(1), ..Default::default() }),
            "a withdrawal is a change, and must be published"
        );

        let state = m.state();
        assert_eq!(state.artist, None, "the previous track's artist must not survive");
        assert_eq!(state.title.as_deref(), Some("Vous ecoutez Radio X"));
        assert_eq!(state.origin.as_deref(), Some(ORIGIN_ICY));
        assert_eq!(state.provenance.fields.get("title").map(String::as_str), Some(ORIGIN_ICY));
        assert!(state.provenance.derived.is_empty(), "nobody reworked this string");
        assert!(state.provenance.misses.is_empty(), "and nobody searched either");

        // Idempotent: a second withdrawal changes nothing, hence publishes
        // nothing. Without it, a station announcing nothing but its own name
        // would send a frame to every display at every repetition.
        assert!(!m.add("musicbrainz", Enrichment { identity: json!(1), ..Default::default() }));
    }

    #[test]
    fn a_lone_position_or_duration_is_still_counted_as_no_response() {
        // The boundary of the withdrawal, and it is not decoration: neither of
        // those two makes a displayable enrichment, but losing one's text by
        // sending a position would be a trap set for a future plugin.
        let mut m = Metadata::new(vec!["musicbrainz".into()]);
        m.set_identity(Some(json!(1)));
        assert!(m.add(
            "musicbrainz",
            Enrichment { identity: json!(1), title: Some("So What".into()), ..Default::default() }
        ));
        assert!(!m.add(
            "musicbrainz",
            Enrichment { identity: json!(1), position_s: Some(42), ..Default::default() }
        ));
        assert_eq!(m.state().title.as_deref(), Some("So What"), "a position costs no text");
        assert!(!m.add(
            "musicbrainz",
            Enrichment { identity: json!(1), duration_s: Some(300), ..Default::default() }
        ));
        assert_eq!(m.state().title.as_deref(), Some("So What"), "and neither does a duration");
    }

    #[test]
    fn a_contributor_fetching_elsewhere_remains_the_source() {
        // The check on the rule above: without `derived_from`, nothing
        // changes. A lookup by TOC or a cover search *is* the source, and
        // noting it as a rework would be the opposite defect.
        let mut m = Metadata::new(vec!["musicbrainz".into()]);
        m.set_identity(Some(json!(1)));
        assert!(m.add(
            "musicbrainz",
            Enrichment {
                identity: json!(1),
                artist: Some("Miles Davis".into()),
                title: Some("So What".into()),
                ..Default::default()
            }
        ));

        let state = m.state();
        assert_eq!(state.origin.as_deref(), Some("musicbrainz"));
        assert_eq!(state.provenance.fields.get("title").map(String::as_str), Some("musicbrainz"));
        assert!(state.provenance.derived.is_empty(), "nothing was reworked");
    }

    #[test]
    fn a_contributor_that_searched_without_finding_is_named_separately() {
        // The second part of the request: "musicbrainz has no album for this
        // track" is not "musicbrainz was never asked", and the absence alone
        // conflated the two.
        let mut m = Metadata::new(vec!["musicbrainz".into()]);
        m.set_identity(Some(json!(1)));
        m.set_icy("Miles Davis - So What".into());
        // An entirely empty enrichment is refused... unless it declares it
        // searched. That is the only exemption, and it is the mechanism.
        assert!(
            !m.add("musicbrainz", Enrichment { identity: json!(1), ..Default::default() }),
            "an empty enrichment without `searched` remains refused"
        );
        assert!(m.add(
            "musicbrainz",
            Enrichment { identity: json!(1), searched: true, fill_only: true, ..Default::default() }
        ));

        let state = m.state();
        assert_eq!(state.provenance.misses, vec!["musicbrainz".to_string()]);
        // And it won nothing and erased nothing: the ICY title still holds.
        assert_eq!(state.title.as_deref(), Some("Miles Davis - So What"));
        assert_eq!(state.origin.as_deref(), Some(ORIGIN_ICY));
        assert_eq!(state.provenance.fields.get("title").map(String::as_str), Some(ORIGIN_ICY));
    }

    #[test]
    fn a_plugin_keeps_priority_even_over_a_more_recent_icy_title() {
        // Owner decision: a `metadata` plugin has priority over the ICY
        // **under all circumstances**. An earlier version erased the
        // enrichments at every new ICY title (that one proving the track had
        // changed), which made the display go through the ICY form —
        // "Title - ARTIST" on these streams — before correction by the
        // plugin, twice per track.
        //
        // A trade-off owned, verified here: at a track change, it is the
        // previous title that stays displayed until the plugin's next frame.
        let mut m = Metadata::new(vec!["ouifm".into()]);
        let id = json!({"kind": "stream", "url": "http://ouifm3"});
        m.set_identity(Some(id.clone()));
        m.set_icy("Made Up - TAHITI 80".into());
        m.add("ouifm", enrichment(id.clone(), "TAHITI 80", "MADE UP"));
        assert_eq!(m.state().origin.as_deref(), Some("ouifm"));

        // Next track: the stream announces it, the plugin has not spoken yet.
        assert!(m.set_icy("Fade To Grey - VISAGE".into()), "the ICY is indeed retained");
        let state = m.state();
        assert_eq!(state.origin.as_deref(), Some("ouifm"), "the plugin keeps the upper hand");
        assert_eq!(state.artist.as_deref(), Some("TAHITI 80"));

        // Then the plugin catches up.
        m.add("ouifm", enrichment(id, "VISAGE", "FADE TO GREY"));
        assert_eq!(m.state().artist.as_deref(), Some("VISAGE"));
    }

    #[test]
    fn the_icy_takes_over_again_when_the_station_changes() {
        // The plugin's priority only holds for **what is playing**: changing
        // stations changes the identity, which wipes the slate clean. Without
        // that, one station's title would follow onto the next.
        let mut m = Metadata::new(vec!["ouifm".into()]);
        let station_one = json!({"kind": "stream", "url": "http://ouifm3"});
        m.set_identity(Some(station_one.clone()));
        m.add("ouifm", enrichment(station_one, "TAHITI 80", "MADE UP"));
        assert_eq!(m.state().origin.as_deref(), Some("ouifm"));

        m.set_identity(Some(json!({"kind": "stream", "url": "http://fip"})));
        m.set_icy("Miles Davis - So What".into());
        let state = m.state();
        assert_eq!(state.origin.as_deref(), Some("icy"));
        assert_eq!(state.title.as_deref(), Some("Miles Davis - So What"));
    }

    #[test]
    fn a_repeated_icy_triggers_nothing() {
        let mut m = Metadata::new(vec![]);
        m.set_identity(Some(json!(1)));
        assert!(m.set_icy("Miles Davis - So What".into()));
        assert!(!m.set_icy("Miles Davis - So What".into()), "Icecast repeats the same header");
    }

    #[test]
    fn an_icy_title_is_retained_even_without_a_declared_identity() {
        // The ICY layer does not depend on the Source's goodwill: a Source
        // that declares no identity (third-party plugin, binary not yet
        // updated) must not deprive the device of the only layer that works
        // without a plugin. It is up to the core to know whether something is
        // playing — see `Core::handle_icy_title`, which relies on
        // `expecting_stream`.
        let mut m = Metadata::new(vec![]);
        assert!(m.set_icy("Miles Davis - So What".into()));
        assert_eq!(m.state().title.as_deref(), Some("Miles Davis - So What"));
        assert_eq!(m.state().origin.as_deref(), Some("icy"));
    }

    #[test]
    fn an_identical_enrichment_triggers_nothing() {
        // A plugin that reopens its connection to a remote stream re-emits
        // the current track every time. Without this deduplication, each
        // repetition caused a write towards the displays and an SSE frame
        // towards every connected browser.
        let mut m = Metadata::new(vec!["ouifm".into()]);
        let id = json!({"url": "one"});
        m.set_identity(Some(id.clone()));
        assert!(m.add("ouifm", enrichment(id.clone(), "A", "T")));
        assert!(!m.add("ouifm", enrichment(id.clone(), "A", "T")));
        // Whitespace being normalized on entry, the same information under
        // another form does not get through either.
        let with_blanks = Enrichment {
            identity: id.clone(),
            artist: Some("  A ".into()),
            title: Some("T".into()),
            ..Default::default()
        };
        assert!(!m.add("ouifm", with_blanks));
        // A real change gets through.
        assert!(m.add("ouifm", enrichment(id, "A", "Other title")));
    }

    #[test]
    fn the_winner_is_the_highest_priority_plugin_that_answered() {
        // That is what the core logs: naming the last to have answered would
        // lie in the only case where that log is consulted — a dubious
        // display to attribute.
        let mut m = Metadata::new(vec!["priority".into(), "secondary".into()]);
        let id = json!({"url": "one"});
        m.set_identity(Some(id.clone()));
        assert_eq!(m.winner(), None);
        m.add("secondary", enrichment(id.clone(), "Second", "T"));
        assert_eq!(m.winner(), Some("secondary"));
        m.add("priority", enrichment(id.clone(), "First", "T"));
        assert_eq!(m.winner(), Some("priority"));
        // A new answer from the lower-priority one does not change the winner.
        m.add("secondary", enrichment(id, "Second again", "T"));
        assert_eq!(m.winner(), Some("priority"));
    }

    /// The position follows the **winner** of the arbitration, like the rest
    /// of the track: a lower-priority plugin held in reserve must not impose
    /// its own clock.
    #[test]
    fn the_position_comes_from_the_winner() {
        let mut m = Metadata::new(vec!["radiofrance".into(), "ouifm".into()]);
        m.set_identity(Some(json!({"url": "https://fip"})));
        m.add(
            "ouifm",
            Enrichment {
                identity: json!({"url": "https://fip"}),
                title: Some("from ouifm".into()),
                position_s: Some(200),
                ..Default::default()
            },
        );
        assert_eq!(m.position_s(), Some(200));
        m.add(
            "radiofrance",
            Enrichment {
                identity: json!({"url": "https://fip"}),
                title: Some("from radiofrance".into()),
                position_s: Some(12),
                ..Default::default()
            },
        );
        assert_eq!(m.position_s(), Some(12), "the highest priority wins");
    }

    #[test]
    fn without_enrichment_there_is_no_position() {
        let m = Metadata::new(vec!["radiofrance".into()]);
        assert_eq!(m.position_s(), None);
    }

    #[test]
    fn the_raw_string_survives_the_enrichment_that_overrides_it() {
        // The property the whole feature depends on. A radio's identity is
        // the stream URL: it does not change between two tracks, and
        // `set_icy` deliberately does not erase the enrichments. So without
        // this field, a plugin that once wrote an artist would never see the
        // ICY string again, and could not split anything anymore — "it works
        // once".
        let mut m = Metadata::new(vec!["musicbrainz".to_string()]);
        let identity = serde_json::json!({ "kind": "stream", "url": "http://example/stream.mp3" });
        m.set_identity(Some(identity.clone()));
        assert!(m.set_icy("Miles Davis - So What".into()));

        // The plugin corrects, by overriding: the composed title becomes its
        // own.
        assert!(m.add(
            "musicbrainz",
            ritornello_proto::Enrichment {
                identity: identity.clone(),
                artist: Some("Miles Davis".into()),
                title: Some("So What".into()),
                ..Default::default()
            }
        ));
        assert_eq!(m.known().title.as_deref(), Some("So What"));

        // Next track, same station: the previous enrichment is still there
        // (identity unchanged), but the raw string must be the new one.
        assert!(m.set_icy("John Coltrane - Naima".into()));
        assert_eq!(
            m.known().stream_title.as_deref(),
            Some("John Coltrane - Naima"),
            "the raw one must follow the stream, not the composition"
        );
    }

    #[test]
    fn without_icy_the_field_stays_empty() {
        let m = Metadata::new(vec![]);
        assert_eq!(m.known().stream_title, None);
    }

    #[test]
    fn the_state_carries_the_source_and_the_duration() {
        let mut m = Metadata::new(vec!["ouifm".into()]);
        let id = json!({"url": "one"});
        m.set_identity(Some(id.clone()));
        m.add(
            "ouifm",
            Enrichment {
                identity: id,
                artist: Some("Shaka Ponk".into()),
                title: Some("Wanna Get Free".into()),
                album: None,
                duration_s: Some(214),
                position_s: None,
                ..Default::default()
            },
        );
        let state = m.state();
        assert_eq!(state.duration_s, Some(214));
    }
}
