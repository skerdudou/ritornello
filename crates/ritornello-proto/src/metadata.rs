//! Protocol of the `metadata` kind: the core announces what is playing, a plugin
//! sends back what it knows about it.
//!
//! Unlike `source` (request/response correlated by `id`) and `display` or
//! `input` (one-way), this protocol is **bidirectional and uncorrelated**: each
//! side emits when it has something to say. The core asks for nothing, because
//! it has no way of knowing whether a plugin will be able to answer nor how
//! long it will take; the plugin does not wait for a reply, because an
//! enrichment is neither accepted nor refused, it is simply retained or
//! expired.
//!
//! The safeguard against staleness is the **identity echo**: an enrichment
//! carries the identity it relates to, and the core discards the one that no
//! longer matches what is playing. Without this echo, a plugin's slow answer
//! about the previous track would overwrite the current track.

use serde::{Deserialize, Serialize};

/// What the Source says about what it is playing, carried alongside the view.
///
/// Three states are needed, which is why this type is an enum rather than an
/// `Option`: the absence of the field in a frame ("this reply says nothing
/// about the identity") must not be confused with `Nothing` ("nothing is
/// playing anymore"), and serde maps `null` and absence to the same value for
/// an `Option`. The three cases are therefore: field absent, `Playing`,
/// `Nothing`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
// `value` and not `identity`: nested inside `SourceMessage.identity`, the latter
// would yield `"identity":{"state":"Playing","identity":{…}}` — the protocol is
// meant to be readable by eye in a `journalctl`.
#[serde(tag = "state", content = "value")]
pub enum IdentityUpdate {
    /// **Opaque** identity, produced by the Source, never interpreted by the
    /// core — the same principle as the opaque JSON of the `admin` protocol.
    /// The core merely compares two identities for equality.
    Playing(serde_json::Value),
    /// Nothing is playing anymore: the core forgets the current identity and
    /// tells the `metadata` plugins so they stop their work.
    Nothing,
}

/// The partial state of the track, as a plugin needs to see it.
///
/// A dedicated type rather than `Track`: the latter carries `cover_href` and
/// `cover_origin`, which are URLs **local to the device** — they mean nothing
/// to a plugin and would invite it to believe it can read them.
///
/// A field at `None` is a field nobody has filled in yet. This is what lets a
/// plugin work only on what is missing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Known {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_s: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<u16>,
    /// A cover is **already held**. A boolean, never the image: a plugin does
    /// not need to see it to decide whether it should look for one, and
    /// transmitting it would bloat every frame for nothing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cover: bool,
    // No `links` here, and that is not an oversight: `Known` exists so that a
    // plugin works only on what is missing, yet none of ours *searches* for
    // links — it copies those from the reply it already reads. The field would
    // change none of their decisions.
    /// What the **stream itself** announced, raw: neither split, nor composed,
    /// nor arbitrated.
    ///
    /// Not a repeat of `title`. `title` is the result of an arbitration between
    /// several contributors and may therefore come from a plugin; this field
    /// is a fact from a single emitter, the station.
    ///
    /// It exists because only the raw form can be **re-split**, and a plugin
    /// needs to see it again even after having itself overwritten the composed
    /// title. A radio's identity is the URL of its stream, so it does not
    /// change from one track to the next: the staleness safeguard of
    /// `Metadata::add` expires nothing, and `set_icy` does not erase the
    /// enrichments. Without this field, a plugin that corrects once would never
    /// again see what the station announces.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream_title: Option<String>,
}

impl Known {
    /// True if nobody has filled anything in yet.
    ///
    /// Serves as `skip_serializing_if`: a frame that says nothing about what is
    /// known must stay byte-for-byte identical to what it was before this
    /// work, and the protocol is meant to be readable by eye in a journalctl.
    /// `year` is part of it, and forgetting it would be a silent loss: this
    /// predicate is the `skip_serializing_if` of `NowPlaying::known`, so a
    /// `Known` judged empty **disappears from the frame**. A year known on its
    /// own would never reach the plugins.
    pub fn is_empty(&self) -> bool {
        self.artist.is_none()
            && self.title.is_none()
            && self.album.is_none()
            && self.duration_s.is_none()
            && self.year.is_none()
            && !self.cover
    }
}

/// What a contributor found as a cover, leaving it to the core to go and
/// fetch it.
///
/// Two **explicitly distinct** forms rather than a string the core would
/// guess at: the path serves the `folder.jpg` sitting on a share, which
/// already exists on disk — nothing to extract, no temporary file.
///
/// Never bytes: the plugin channel stays textual, hence readable by eye in a
/// `journalctl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CoverRef {
    /// External URL, to download. `https` only, towards a host name.
    Url { url: String },
    /// Absolute path of an image file already present on disk.
    Path { path: String },
}

/// Accepted extensions for a `CoverRef::Path`.
const IMAGE_EXTENSIONS: [&str; 4] = ["jpg", "jpeg", "png", "webp"];

impl CoverRef {
    /// Normalizes and **validates**. `None` = to be discarded.
    ///
    /// These values arrive from another process and the core will act on them:
    /// they must be treated as input, not as trusted data.
    ///
    /// Public, because a cover enters the core through **two** channels: a
    /// plugin's enrichment (`Enrichment::cleaned`, just below) and a Source's
    /// frame (`SourceMessage::cover`). Private, this method covered only the
    /// first — the layer documented as owner of the shape validation therefore
    /// owned it on only half of its inputs, and the second channel rested
    /// entirely on the core's own checks.
    pub fn validated(self) -> Option<Self> {
        match self {
            Self::Url { url } => {
                let url = url.trim();
                let rest = url.strip_prefix("https://")?;
                let host = rest.split(['/', '?', '#']).next().unwrap_or("");
                if host.is_empty() || host.contains('@') {
                    return None;
                }
                // A literal IP address is refused: a host name, and nothing
                // else. `[::1]` is ruled out by the bracket, `192.168.1.1` by
                // the fact that all its labels are numeric.
                let without_port = host.split(':').next().unwrap_or("");
                if without_port.starts_with('[')
                    || (!without_port.is_empty()
                        && without_port.split('.').all(|l| !l.is_empty() && l.chars().all(|c| c.is_ascii_digit())))
                {
                    return None;
                }
                if !without_port.contains('.') {
                    return None;
                }
                Some(Self::Url { url: url.to_string() })
            }
            Self::Path { path } => {
                let path = path.trim();
                if !path.starts_with('/') {
                    return None;
                }
                let ext = path.rsplit_once('.')?.1.to_ascii_lowercase();
                IMAGE_EXTENSIONS.contains(&ext.as_str()).then(|| Self::Path { path: path.to_string() })
            }
        }
    }
}

/// A link to the listening platform where this track can be found.
///
/// A closed enum, and not a `(name, url)` pair: this is **the** security
/// decision of this type. The variant names the platform, and
/// [`Self::validated`] then enforces the host that matches it. A third-party
/// source therefore cannot make the UI display a clickable link to a domain of
/// its choosing — at worst it lies about its own domain, which is the risk we
/// already accept by believing it about the title.
///
/// With a free `platform: String` field, a `{"platform":"deezer",
/// "url":"https://elsewhere.example/x"}` would be rendered as is: the check
/// would have nothing left to hold on to.
///
/// Adding a platform is a modification of this file, deliberately: it forces
/// one to write its host here, next to the others.
///
/// **Consequence of the internally-tagged enum, accepted:** a frame naming a
/// platform this file does not know does not lose that one link, it makes the
/// deserialization of the **whole** enrichment fail — `serde` has no fallback
/// variant to give it, and a `#[serde(other)]` would add one that has neither
/// an allowed host nor an icon. This is accepted because core and plugins are
/// deployed **together**, from a single package: a plugin cannot end up ahead
/// of the core that reads it. The day that is no longer true, a
/// `Vec<serde_json::Value>` stripped link by link would be needed, and only on
/// that day — adding it in advance would cost the typing that gives this type
/// all its value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "platform", rename_all = "snake_case")]
pub enum Link {
    Youtube { url: String },
    Deezer { url: String },
    AppleMusic { url: String },
}

impl Link {
    /// The hosts allowed for this platform, and **nothing else**.
    ///
    /// A list and not a single host, because one platform publishes itself
    /// under several names and those really are its links: `youtu.be` is the
    /// shortened form YouTube itself emits, `music.youtube.com` its music
    /// variant. They must therefore work, and **with the same icon** — which
    /// comes for free, since it is the variant and not the host that picks the
    /// icon on the UI side.
    ///
    /// Radio France today emits only `www.youtube.com` (measured on
    /// 2026-08-27); the other forms are allowed in advance rather than after a
    /// silent failure the day the third party changes its mind.
    ///
    /// **This list is the security boundary of the type.** Adding a name to it
    /// is a decision, not a formality: everything listed becomes a link the
    /// device will render clickable on the word of a third party.
    fn allowed_hosts(&self) -> &'static [&'static str] {
        match self {
            Self::Youtube { .. } => {
                &["www.youtube.com", "youtube.com", "m.youtube.com", "music.youtube.com", "youtu.be"]
            }
            Self::Deezer { .. } => &["www.deezer.com", "deezer.com"],
            Self::AppleMusic { .. } => &["music.apple.com"],
        }
    }

    fn url(&self) -> &str {
        match self {
            Self::Youtube { url } | Self::Deezer { url } | Self::AppleMusic { url } => url,
        }
    }

    /// Normalizes and **validates**. `None` = to be discarded.
    ///
    /// The comparison bears on the **authority** and not on a string prefix:
    /// `https://www.deezer.com.evil.example/x` does have the real domain as a
    /// prefix without being one. It is the same mistake the OUI FM plugin
    /// documented for its image host, and it is closed here for everyone at
    /// once.
    ///
    /// The port is refused, and so is userinfo (`@`): `https://
    /// www.deezer.com@evil.example/` has `evil.example` as its real host.
    pub fn validated(self) -> Option<Self> {
        let allowed = self.allowed_hosts();
        let url = self.url().trim().to_string();
        let rest = url.strip_prefix("https://")?;
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        // Strict equality against each allowed name, never a suffix:
        // `evil-youtube.com` and `youtube.com.evil.example` both fail, where an
        // `ends_with` would let the first through and a `starts_with` the
        // second.
        if !allowed.contains(&authority) {
            return None;
        }
        Some(match self {
            Self::Youtube { .. } => Self::Youtube { url },
            Self::Deezer { .. } => Self::Deezer { url },
            Self::AppleMusic { .. } => Self::AppleMusic { url },
        })
    }
}

/// Plausibility bounds of a year, on both sides.
///
/// These values come from a third party or from an arbitrary file tag. A year
/// of 0 or 90210 teaches nothing and uglifies the screen; refusing it costs
/// only what it was worth.
const YEAR_MIN: u16 = 1000;
const YEAR_MAX: u16 = 2999;

/// Reads a year in the forms our sources return. `None` = to be discarded.
///
/// Three forms measured, hence the existence of this function rather than a
/// `parse()` at each caller: MusicBrainz returns `"1987"` or `"2017-06-23"`,
/// the Radio France schedule returns the **number** 1952, and file tags return
/// a bit of everything, `"1972-00-00"` included.
///
/// The rule bears on the **length** of the numeric head, and not on its value:
/// 4 digits is the year; 8 digits is a compact `YYYYMMDD` (which ID3 tags
/// write, `TDRC` allowing the tight form) of which we keep the first four; any
/// other length is discarded. Without this rule, `"19590817"` yielded `None`
/// (out of bounds) and above all `"90210"` yielded 9021 — the upper bound does
/// not catch a postal code, it only truncates the number it forms.
pub fn valid_year(raw: &str) -> Option<u16> {
    let head: String = raw.trim().chars().take_while(char::is_ascii_digit).collect();
    let head = match head.len() {
        4 => head.as_str(),
        8 => &head[..4],
        _ => return None,
    };
    let year = head.parse::<u16>().ok()?;
    (YEAR_MIN..=YEAR_MAX).contains(&year).then_some(year)
}

/// Core → plugin. Emitted at each change of what is playing, and at stop
/// (`identity: None`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NowPlaying {
    /// Name of the active Source (`"radio"`, `"cd"`…), so that a plugin can
    /// stay silent from the outset on a source it does not handle, without
    /// having to inspect the shape of the identity.
    pub source: String,
    /// `None` = nothing is playing anymore.
    #[serde(default)]
    pub identity: Option<serde_json::Value>,
    /// What is **already known** of the track, all tiers combined.
    ///
    /// `#[serde(default)]`: a frame written by an earlier binary reads back,
    /// and a plugin that ignores the field works exactly as before — this is
    /// what makes the overhaul deployable plugin by plugin.
    /// `skip_serializing_if`: as long as nothing is known, the frame stays
    /// byte-for-byte identical to what it was before this field.
    #[serde(default, skip_serializing_if = "Known::is_empty")]
    pub known: Known,
}

/// Plugin → core. Emitted when the plugin learns something.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Enrichment {
    /// **Echo** of the identity concerned: the staleness safeguard.
    pub identity: serde_json::Value,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub album: Option<String>,
    #[serde(default)]
    pub duration_s: Option<u32>,
    /// Release year. Validated by [`valid_year`] at the contributor, and
    /// re-bounded here by [`Self::cleaned`]: the value arrives from another
    /// process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<u16>,
    /// The listening platforms where this track can be found.
    ///
    /// A list and not an `Option`: a contributor may know several at once
    /// (OUI FM returns Deezer **and** Apple Music in the same frame), and the
    /// empty list already says "none".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    /// Elapsed within the track **at the time of emission**, in seconds.
    ///
    /// A relative elapsed rather than an absolute timestamp: nothing to
    /// synchronize between two clocks, and it is the convention of
    /// `duration_s` just above. The core anchors it on reception and advances
    /// it itself afterwards (see `Core::refresh_position`).
    #[serde(default)]
    pub position_s: Option<u32>,
    /// The cover this contributor found.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover: Option<CoverRef>,
    /// A ready-made thumbnail for the cover above, if this contributor has
    /// one.
    ///
    /// **Optional, and the pair is not two covers.** `cover` stays the
    /// subject — the full-size image, the one an enlarged view wants. This is
    /// the same image, already reduced, offered so the appliance does not
    /// re-encode what is already the right size: Cover Art Archive's
    /// `front-500` weighs 73 KiB against 2.5 MiB for the original, and 73 KiB
    /// is almost exactly what our own encoder would produce anyway.
    ///
    /// **It is always used; the rule decides only whether it is touched.**
    /// Within the rule — no wider than `cover_max_edge_px`, no heavier than
    /// `cover_passthrough_max_ko`, the same rule that decides whether any
    /// cover is left alone — it is served byte for byte. Outside it, it is
    /// re-encoded **from itself**, and the full size is still not fetched for
    /// that. Missing the rule costs a re-encoding, never the field. One
    /// mechanism, two uses.
    ///
    /// The same serde attributes as `cover` just above, deliberately: a
    /// plugin that ignores the field emits exactly the frame it emitted
    /// before, and absence stays absence on the wire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_thumb: Option<CoverRef>,
    /// This contributor only **fills in**: it replaces no field already set.
    ///
    /// Default `false` = it overwrites, which is the project's current rule ("a
    /// plugin takes precedence over ICY and over file tags under all
    /// circumstances") and what avoids touching the shipped plugins.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fill_only: bool,
    /// This contributor **searched** for this track.
    ///
    /// Set along with the fields it found, or alone when it found nothing —
    /// and that is the only case where an entirely empty enrichment is
    /// accepted by the core, which refuses the others. This is what lets the
    /// screen distinguish "MusicBrainz has no album for this track" from
    /// "MusicBrainz was never queried", two situations that absence alone
    /// conflated.
    ///
    /// An enrichment empty in this way **takes part in nothing**: it cannot
    /// win the arbitration (it says nothing about the text) nor fill anything
    /// in (all its fields are absent). It only adds a line to
    /// `Provenance::misses`.
    ///
    /// Not to be confused with "I could not search": a service outage is not
    /// declared here. The contributor then emits nothing at all, and retries.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub searched: bool,
    /// This contributor **re-reads** information already present, it does not
    /// bring it: the name of its source.
    ///
    /// **The concrete case is splitting an ICY header.** A radio announces
    /// `"Artist - Title"` as a single string; `musicbrainz` splits it according
    /// to a pattern learned for that station, and checks the split against its
    /// database. The result was attributed to it — "Title: musicbrainz" — even
    /// though it taught nobody anything: the information comes from the
    /// station, it merely read it differently. The owner reported it on a
    /// radio with no metadata plugin, where the only real contributor was the
    /// ICY.
    ///
    /// The core therefore attributes the fields to **that source**, and notes
    /// separately who reworked them (see `Provenance::derived`). One no longer
    /// replaces the other.
    ///
    /// `None` — the default — for a contributor that fetches the information
    /// elsewhere: a lookup by TOC, a cover search. There, it *is* the source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_from: Option<String>,
}

impl Enrichment {
    /// Brings any empty or blank string back to `None`.
    ///
    /// A plugin that does not know the artist may just as well send `null` as
    /// an empty string — both say the same thing. Normalizing here spares the
    /// rest of the core from handling two cases, and above all prevents a
    /// `title: ""` from counting as an answer and blocking a lower-priority
    /// plugin that does know the title (see `is_empty`).
    pub fn cleaned(mut self) -> Self {
        fn clear(field: &mut Option<String>) {
            if field.as_deref().is_some_and(|s| s.trim().is_empty()) {
                *field = None;
            } else if let Some(s) = field {
                *s = s.trim().to_string();
            }
        }
        clear(&mut self.artist);
        clear(&mut self.title);
        clear(&mut self.album);
        self.cover = self.cover.take().and_then(CoverRef::validated);
        // The twin line, and it goes through the **same** `validated`: the two
        // halves of a pair are the same kind of value, arriving from the same
        // process, and a second grammar written here would eventually judge
        // them differently.
        self.cover_thumb = self.cover_thumb.take().and_then(CoverRef::validated);
        // Re-bounded here even though the contributor is supposed to have done
        // it: this value crosses a socket, and this layer is the one documented
        // as owner of the shape validation.
        self.year = self.year.filter(|a| (YEAR_MIN..=YEAR_MAX).contains(a));
        // Each link goes through its own host validation; those that fail it
        // are **discarded one by one**, not the whole list: a dubious
        // `deezerId` must not cost the YouTube link that accompanies it.
        self.links = self.links.drain(..).filter_map(Link::validated).collect();
        self
    }

    /// True if the enrichment brings no information.
    ///
    /// To be called **after** `cleaned`. Such an enrichment counts as a
    /// non-answer in the arbitration: a plugin that recognizes the identity but
    /// has learned nothing yet must not block a lower-priority plugin.
    pub fn is_empty(&self) -> bool {
        self.artist.is_none() && self.title.is_none() && self.album.is_none()
    }
}

/// What is displayable of the current track.
///
/// `origin` says **who** provided the information (`"icy"` or the name of the
/// winning plugin): without it, a dubious display would be attributable to
/// nobody, and that is exactly the question one asks in front of a wrong
/// title.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Track {
    pub artist: Option<String>,
    pub title: Option<String>,
    pub album: Option<String>,
    pub duration_s: Option<u32>,
    /// Release year, when a contributor knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub year: Option<u16>,
    /// The listening platforms, already validated (see [`Link::validated`]).
    ///
    /// Travels in the common payload rather than through a channel reserved
    /// for the UI: that is the project's convention, each display composes
    /// what it knows how to show. A text display ignores it, the web UI turns
    /// it into buttons.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    pub origin: Option<String>,
    /// **Local** URL of the cover, to be put as is into a `src`. Always of the
    /// form `/api/cover/{key}`: the UI never contacts the outside.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_href: Option<String>,
    /// Who provided this cover: the name of the Source, `"tags"`, or the name
    /// of the plugin. A second origin, because the text and the image may come
    /// from two different contributors.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_origin: Option<String>,
    /// Who provided what, **field by field**, and who searched without
    /// finding.
    ///
    /// `origin` and `cover_origin` stay: they say the contributor of the
    /// **text** and that of the **image**, which a three-line display can show
    /// and out of which the UI makes its two badges. This field goes further,
    /// because the text itself is composed by several hands — the winner, then
    /// the `fill_only` ones that fill in, then the year and the links that are
    /// taken from everywhere — and `origin` names only the first.
    #[serde(default, skip_serializing_if = "Provenance::is_empty")]
    pub provenance: Provenance,
}

/// Where each piece of what is displayed comes from.
///
/// **What this answers, and that nothing else answered**: "why is this title
/// wrong?". `origin` names the contributor of the text block, but the year may
/// come from another, the cover from a third, and a fourth may have searched
/// in vain — three facts the screen carried nowhere.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    /// The contributor retained for each **filled** field, by field name:
    /// `artist`, `title`, `album`, `year`, `duration`, `links`, `cover`.
    ///
    /// A map rather than a seven-field structure: consumers iterate over it to
    /// display it, none tests one field in particular, and a map does not
    /// break when a field is added to the track. `BTreeMap` and not `HashMap`:
    /// the iteration order is then stable, hence the serialized frame too —
    /// and the core deduplicates its frames by equality.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub fields: std::collections::BTreeMap<String, String>,
    /// The plugins that **searched and found nothing** for this track.
    ///
    /// Distinct from an absence from the map above: "musicbrainz did not
    /// provide the album" is also true when it was never queried, and that is
    /// very different information for whoever wonders why the screen is
    /// incomplete. A plugin declares it through `Enrichment::searched`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub misses: Vec<String>,
    /// Who **reworked** a field without being its source, by field name.
    ///
    /// Complements `fields` instead of replacing it: "Title: icy, split by
    /// musicbrainz" states both facts, where naming only the splitter lost one
    /// — and the most important one, the one that answers "where does this
    /// information come from". See `Enrichment::derived_from`.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub derived: std::collections::BTreeMap<String, String>,
}

impl Provenance {
    /// True when there is nothing to say: serves the `skip_serializing_if` of
    /// `Track::provenance`, so that no existing frame changes shape.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty() && self.misses.is_empty() && self.derived.is_empty()
    }
}

/// A transient overlay the appliance is showing right now, carrying **both**
/// the raw value and the resolved words: a display can draw a volume gauge
/// from `level`, or simply print `text`, without needing a catalog of its
/// own.
///
/// `remaining_ms` is informative. The core alone owns the deadline — it
/// publishes a frame when the overlay expires — so a display may animate a
/// countdown but never decides when the overlay ends.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Overlay {
    /// Volume/mute overlay.
    Volume { level: u8, muted: bool, text: String, remaining_ms: u32 },
    /// Pending tens offset being composed on the remote (`+10`, `+20`).
    Tens { offset: u8, text: String, remaining_ms: u32 },
    /// Ephemeral message from a source ("empty preset").
    Message { text: String, remaining_ms: u32 },
}

/// Equality **deliberately hand-written**: it ignores `remaining_ms`.
///
/// Two overlays that differ only by the remaining time describe the same
/// screen, and `Core::publish_state` deduplicates frames by equality. An
/// automatic derive would make every redundant refresh pass for a change —
/// several paths of the core refresh for the same event — and every display
/// would reprint the same thing.
///
/// Written here, on `Overlay`, and not on `PlayerState`: at the payload level
/// one would have to compare all the other fields by hand just to treat
/// specially one field nested in an enum under an `Option`, and every field
/// added later would be a potential oversight.
impl PartialEq for Overlay {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Volume { level: a, muted: ma, text: ta, .. },
                Self::Volume { level: b, muted: mb, text: tb, .. },
            ) => a == b && ma == mb && ta == tb,
            (Self::Tens { offset: a, text: ta, .. }, Self::Tens { offset: b, text: tb, .. }) => {
                a == b && ta == tb
            }
            (Self::Message { text: ta, .. }, Self::Message { text: tb, .. }) => ta == tb,
            _ => false,
        }
    }
}

impl Overlay {
    /// Replaces the remaining time, computed at publication from the deadline
    /// the core holds. The `remaining_ms` stored in `self` is therefore never
    /// read — and since equality ignores it, refreshing it does not defeat the
    /// frame deduplication.
    #[must_use]
    pub fn with_remaining(self, remaining_ms: u32) -> Self {
        match self {
            Self::Volume { level, muted, text, .. } => Self::Volume { level, muted, text, remaining_ms },
            Self::Tens { offset, text, .. } => Self::Tens { offset, text, remaining_ms },
            Self::Message { text, .. } => Self::Message { text, remaining_ms },
        }
    }
}

/// What the player is doing, in one word. `Stopped` by default: knowing
/// nothing is playing nothing — the same convention as `can_eject`, where the
/// absence of information equals the absence of capability.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Playback {
    #[default]
    Stopped,
    Playing,
    Paused,
}

impl Playback {
    /// Serves the field's `skip_serializing_if`: the default value does not
    /// travel, so existing frames stay byte-identical. A method and not a
    /// closure: `skip_serializing_if` requires a function path.
    pub fn is_stopped(&self) -> bool {
        matches!(self, Playback::Stopped)
    }
}

/// Player state broadcast to the SPA: what is volatile, and therefore needs to
/// be **pushed**.
///
/// One state and one channel for everything that moves — active source,
/// volume, mute, standby, and the track when it is known. The `/api/status`
/// route, for its part, carries the navigation contract (which plugins exist,
/// which have an admin page): structurally stable, read once at mount. Mixing
/// volatile data into it would force the SPA to reprobe it in a loop to
/// display a volume.
///
/// The track is **flattened** into the JSON (`serde(flatten)`): the UI
/// receives a flat object, without having to distinguish two levels for the
/// same panel.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PlayerState {
    /// Name of the active Source, so the SPA knows what it is talking about.
    pub source: String,
    pub volume: u8,
    pub muted: bool,
    pub standby: bool,
    /// Numbered key matching what is playing, as the active Source declared it
    /// (radio preset, cd track): this is what the UI's remote highlights.
    /// `None` = nothing is playing, or the Source declared nothing.
    pub preset: Option<u8>,
    /// Number of numbered presets offered by the active Source (stations for
    /// the radio, tracks for the cd), as it declared it. `None` = nothing
    /// declared: the UI falls back on the historical 1-9 grid. `Some(0)` =
    /// nothing to number (cd without a disc): no key.
    pub preset_count: Option<u8>,
    /// Readable name of the preset given by `preset`, as the active Source
    /// declared it (the configured station name for the radio). `None`: the
    /// Source names nothing at that slot (the cd, whose "audio CD" has nothing
    /// to do with a named preset), or nothing is playing. Lives and dies with
    /// `preset` — see `Core::set_identity`.
    pub preset_name: Option<String>,
    /// The appliance's current state as a **resolved sentence**: the status a
    /// source declared ("NO DISC", "AUDIO CD") or the core's standby word.
    /// One slot, because there is never more than one status at a time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// The transient overlay showing right now, if any. Displays render it as
    /// they see fit; the SPA ignores it (it shows the volume in plain sight
    /// and has its own toasts).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overlay: Option<Overlay>,
    /// Where what is playing stands, in seconds, **at the instant of
    /// publication**.
    ///
    /// `None` = nobody has anything to answer: nothing is playing, or it is a
    /// stream no `metadata` plugin tracks. Two providers feed this field
    /// without ever fighting — mpv for finite content, a `metadata` plugin for
    /// a stream — because the context decides which of the two is allowed to
    /// speak (see `Core::refresh_position`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub position_s: Option<u32>,
    /// What the player is doing. Additive, in the idiom of `InputMessage.held`
    /// and `PluginStatus.stalled`: absent from the JSON when it is `Stopped`,
    /// so no existing frame changes and an old frame reads back.
    ///
    /// Distinct from `position_s.is_some()`: a paused playback keeps its
    /// position, and a playing stream may have none.
    #[serde(default, skip_serializing_if = "Playback::is_stopped")]
    pub playback: Playback,
    /// What is playing accepts a seek: this is the `finite` the Source
    /// declared at its `Play`, made visible to consumers.
    ///
    /// A field in its own right rather than a deduction from `duration_s`: the
    /// two notions diverge exactly where it matters — Radio France announces
    /// the duration of a track on a live stream that cannot be rewound, a file
    /// without a duration tag remains seekable.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub seekable: bool,
    /// The active Source has something to eject (see
    /// `SourceMessage::can_eject`): this is what lets the web remote grey out
    /// its Eject key rather than emit a command the Source will silently
    /// discard.
    ///
    /// **False by default**: not knowing is offering nothing — the same
    /// convention as the shutdown capabilities of `system.rs`. A boolean and
    /// not an `Option`: on the consumer side, "the Source declared nothing" and
    /// "the Source cannot eject" call for the same greyed button, and a third
    /// state would have no rendering of its own.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub can_eject: bool,
    /// How this device writes a time and a date, as its owner set it.
    ///
    /// **A rendering preference in the state frame, and it has to be said
    /// why.** A display must never go and fetch anything on the side —
    /// everything it shows arrives through this channel — and the clock it
    /// draws in standby is precisely something it shows. The opposite solution
    /// (the core pushing the **already written** time) was rejected: it would
    /// impose one frame per minute, forever, including when nobody is
    /// watching. Here the value only moves on the user's gesture.
    ///
    /// Additive, in the idiom of the rest of the structure: absent from the
    /// JSON at its default value, so no existing frame changes shape.
    #[serde(default, skip_serializing_if = "Clock::is_default")]
    pub clock: Clock,
    #[serde(flatten)]
    pub track: Track,
}

/// The two time-writing settings, as they travel to the displays.
///
/// Two separate fields because they are two independent choices: the order of
/// a date's components and the 12/24 h format do not vary together from one
/// country to another.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Clock {
    /// The order of a date's components.
    #[serde(default)]
    pub date: DateFormat,
    /// Time on 24 h rather than 12 h.
    ///
    /// **The default is 24 h**, so the field is written as "on 12 h" so that
    /// the default value is `false` and disappears from the JSON — the same
    /// additive mechanism as `playback` or `can_eject`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub twelve_hour: bool,
}

impl Clock {
    /// True for the default value: serves the `skip_serializing_if` of
    /// `PlayerState::clock`.
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}

/// The order of a date's components. Mirror of `state::DateFormat` on the core
/// side, which the protocol cannot import.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DateFormat {
    /// `31/12/2026`
    #[default]
    DayMonthYear,
    /// `2026-12-31`
    YearMonthDay,
    /// `12/31/2026`
    MonthDayYear,
}

impl Track {
    /// True if nothing is known of the track.
    ///
    /// Has callers only in tests, and deliberately so: on the UI side, it is
    /// the SPA that decides what to show of a partial state, and the core has
    /// no reason to decide for it.
    ///
    /// This convention is no longer held by the compiler. It was, by a
    /// `#[cfg(test)]`, back when the structure lived in the core with its
    /// tests; such an attribute does not survive the move into a separate
    /// crate, where it applies only to the compilation of that crate and would
    /// make the method disappear for all the others.
    pub fn is_empty(&self) -> bool {
        self.artist.is_none() && self.title.is_none() && self.album.is_none()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn now_playing_roundtrip_with_identity() {
        let np = NowPlaying {
            source: "radio".into(),
            identity: Some(json!({"kind": "stream", "url": "https://ouifm/ouifm-high.mp3"})),
            known: Known::default(),
        };
        let back: NowPlaying = serde_json::from_str(&serde_json::to_string(&np).unwrap()).unwrap();
        assert_eq!(back, np);
    }

    #[test]
    fn now_playing_roundtrip_without_identity() {
        let np = NowPlaying { source: "cd".into(), identity: None, known: Known::default() };
        let json = serde_json::to_string(&np).unwrap();
        let back: NowPlaying = serde_json::from_str(&json).unwrap();
        assert_eq!(back.identity, None);
    }

    #[test]
    fn enrichment_roundtrip() {
        let e = Enrichment {
            identity: json!({"kind": "disc", "track": 3}),
            artist: Some("Miles Davis".into()),
            title: Some("So What".into()),
            album: Some("Kind of Blue".into()),
            duration_s: Some(545),
            // Non-default values: this test checks a complete round trip, and a
            // field left at its default value would prove nothing about its
            // encoding. The two links cover both forms of the `Vec`.
            year: Some(1959),
            links: vec![
                Link::Youtube { url: "https://www.youtube.com/watch?v=zqNTltOGh5c".into() },
                Link::Deezer { url: "https://www.deezer.com/track/9956167".into() },
            ],
            position_s: None,
            cover: None,
            cover_thumb: None,
            fill_only: false,
            searched: false,
            derived_from: None,
        };
        let back: Enrichment = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(back, e);
    }

    #[test]
    fn enrichment_accepts_absent_fields() {
        // A minimal plugin sends only what it knows: the missing fields must
        // not make the frame fail to read.
        let e: Enrichment = serde_json::from_str(r#"{"identity":{"k":1},"title":"Bikwix"}"#).unwrap();
        assert_eq!(e.title.as_deref(), Some("Bikwix"));
        assert_eq!(e.artist, None);
        assert_eq!(e.duration_s, None);
    }

    #[test]
    fn identity_update_distinguishes_the_three_states() {
        // Playing and Nothing must be distinguishable on the wire, and the
        // absence of the field (tested in source.rs) is a third case.
        let playing = IdentityUpdate::Playing(json!({"kind": "stream"}));
        assert_eq!(
            serde_json::to_string(&playing).unwrap(),
            r#"{"state":"Playing","value":{"kind":"stream"}}"#
        );
        assert_eq!(serde_json::to_string(&IdentityUpdate::Nothing).unwrap(), r#"{"state":"Nothing"}"#);
        let back: IdentityUpdate = serde_json::from_str(r#"{"state":"Nothing"}"#).unwrap();
        assert_eq!(back, IdentityUpdate::Nothing);
    }

    #[test]
    fn a_link_can_only_target_its_own_platform() {
        // THE security property of this type. These URLs come from a third
        // party, and the UI turns them into a clickable link: without this
        // check, a hostile frame places a link to the target of its choice
        // under a trusted icon.
        for bad in [
            // Another platform's host, or any host at all.
            "https://www.deezer.com/track/1",
            "https://evil.example/x",
            // The real domain as a mere string prefix, and as a mere suffix:
            // the two classic mistakes of a check by `starts_with` or
            // `ends_with`.
            "https://www.youtube.com.evil.example/x",
            "https://evil-youtube.com/x",
            // Userinfo confusion: the real host is evil.example.
            "https://www.youtube.com@evil.example/x",
            // Scheme.
            "http://www.youtube.com/watch?v=a",
            "javascript:alert(1)",
            "",
        ] {
            assert!(
                Link::Youtube { url: bad.into() }.validated().is_none(),
                "wrongly accepted: {bad:?}"
            );
        }
    }

    #[test]
    fn youtube_short_forms_are_allowed_and_keep_the_same_icon() {
        // Owner's decision: `youtu.be` must work like `youtube.com`, with the
        // same icon. It comes for free — it is the variant and not the host
        // that picks the icon on the UI side — provided the validation allows
        // the short form, which this test locks in.
        for good in [
            "https://www.youtube.com/watch?v=zIqlKJj9IlY",
            "https://youtube.com/watch?v=a",
            "https://m.youtube.com/watch?v=a",
            "https://music.youtube.com/watch?v=a",
            "https://youtu.be/zIqlKJj9IlY",
        ] {
            let l = Link::Youtube { url: good.into() }.validated();
            assert!(matches!(l, Some(Link::Youtube { .. })), "wrongly refused: {good:?}");
        }
        // And the two other platforms keep their own hosts.
        assert!(Link::Deezer { url: "https://www.deezer.com/track/1".into() }.validated().is_some());
        assert!(Link::Deezer { url: "https://deezer.com/track/1".into() }.validated().is_some());
        assert!(Link::AppleMusic { url: "https://music.apple.com/us/song/1".into() }.validated().is_some());
        // `youtu.be` does not open the door to the other variants.
        assert!(Link::Deezer { url: "https://youtu.be/a".into() }.validated().is_none());
    }

    #[test]
    fn an_invalid_link_does_not_lose_the_others() {
        // A dubious identifier at one provider must not cost the valid link
        // that accompanies it in the same frame.
        let e = Enrichment {
            identity: json!(1),
            links: vec![
                Link::Deezer { url: "https://evil.example/x".into() },
                Link::AppleMusic { url: "https://music.apple.com/us/song/1443171670".into() },
            ],
            ..Default::default()
        }
        .cleaned();
        assert_eq!(
            e.links,
            vec![Link::AppleMusic { url: "https://music.apple.com/us/song/1443171670".into() }]
        );
    }

    #[test]
    fn an_out_of_bounds_year_is_refused() {
        // These values come from arbitrary file tags and from third parties. A
        // year of 0 or 90210 teaches nothing and uglifies the screen.
        for raw in ["0", "999", "3000", "90210", "195", "", "abc", "-1959"] {
            assert_eq!(valid_year(raw), None, "wrongly accepted: {raw:?}");
        }
        // The three forms measured at our sources.
        assert_eq!(valid_year("1987"), Some(1987), "MusicBrainz, year alone");
        assert_eq!(valid_year("2017-06-23"), Some(2017), "MusicBrainz, full date");
        assert_eq!(valid_year("1972-00-00"), Some(1972), "wonky file tag");
        assert_eq!(valid_year("  1959  "), Some(1959), "trimmed");
        // The compact form of ID3 tags (`TDRC` allows `YYYYMMDD`): the numeric
        // head is then 8 digits in a row, and without a rule on the length it
        // yielded `None` for not fitting in a `u16`.
        assert_eq!(valid_year("19590817"), Some(1959), "compact ID3 tag");
        // The trap of naive truncation: keeping "the first four digits"
        // without looking at the length would turn this postal code into the
        // year 9021, and the upper bound would catch nothing.
        assert_eq!(valid_year("90210"), None, "a postal code is not a year");
        // Re-bounding also goes through `cleaned`, the layer that owns the
        // shape validation.
        let e = Enrichment { identity: json!(1), year: Some(90), ..Default::default() }.cleaned();
        assert_eq!(e.year, None);
    }

    #[test]
    fn cleaned_brings_blank_to_none_and_trims() {
        let e = Enrichment {
            identity: json!(1),
            artist: Some("   ".into()),
            title: Some("  So What  ".into()),
            album: Some(String::new()),
            duration_s: None,
            year: None,
            links: Vec::new(),
            position_s: None,
            cover: None,
            cover_thumb: None,
            fill_only: false,
            searched: false,
            derived_from: None,
        }
        .cleaned();
        assert_eq!(e.artist, None);
        assert_eq!(e.title.as_deref(), Some("So What"));
        assert_eq!(e.album, None);
    }

    #[test]
    fn is_empty_only_counts_text_fields() {
        assert!(Enrichment { identity: json!(1), ..Default::default() }.is_empty());
        // A duration alone does not make a displayable enrichment: it is not
        // enough to win the arbitration against a plugin that knows the title.
        let duration_only = Enrichment { identity: json!(1), duration_s: Some(210), ..Default::default() };
        assert!(duration_only.is_empty());
        let artist_only =
            Enrichment { identity: json!(1), artist: Some("FIP".into()), ..Default::default() };
        assert!(!artist_only.is_empty());
    }

    #[test]
    fn overlay_volume_makes_a_json_round_trip() {
        let o = Overlay::Volume { level: 65, muted: false, text: "VOLUME 65 %".into(), remaining_ms: 4200 };
        let json = serde_json::to_string(&o).unwrap();
        // Internal tagging: a flat object, simpler to read on the web side than
        // a {"kind":…,"data":{…}} pair.
        assert!(json.contains("\"kind\":\"volume\""));
        assert!(json.contains("\"level\":65"));
        let back: Overlay = serde_json::from_str(&json).unwrap();
        assert_eq!(back, o);
    }

    #[test]
    fn overlay_tens_and_message_make_a_json_round_trip() {
        let t = Overlay::Tens { offset: 20, text: "PRESELECTION +20".into(), remaining_ms: 3000 };
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains("\"kind\":\"tens\""));
        assert_eq!(serde_json::from_str::<Overlay>(&json).unwrap(), t);

        let m = Overlay::Message { text: "PRESELECTION VIDE".into(), remaining_ms: 5000 };
        let json = serde_json::to_string(&m).unwrap();
        assert!(json.contains("\"kind\":\"message\""));
        assert_eq!(serde_json::from_str::<Overlay>(&json).unwrap(), m);
    }

    #[test]
    fn two_overlays_differing_only_by_remaining_time_are_equal() {
        // The guarantee that protects the deduplication of `publish_state`: two
        // frames that differ only by the remaining time describe the same
        // screen. Without this equality, every redundant refresh would be
        // pushed, and every display would reprint the same thing.
        let a = Overlay::Volume { level: 65, muted: false, text: "VOLUME 65 %".into(), remaining_ms: 4200 };
        let b = Overlay::Volume { level: 65, muted: false, text: "VOLUME 65 %".into(), remaining_ms: 120 };
        assert_eq!(a, b);
    }

    #[test]
    fn an_overlay_that_differs_elsewhere_stays_different() {
        // Safeguard of the equality above: it ignores the remaining time, and
        // nothing else.
        let a = Overlay::Volume { level: 65, muted: false, text: "VOLUME 65 %".into(), remaining_ms: 4200 };
        let b = Overlay::Volume { level: 66, muted: false, text: "VOLUME 66 %".into(), remaining_ms: 4200 };
        assert_ne!(a, b);
        let c = Overlay::Message { text: "X".into(), remaining_ms: 1 };
        let d = Overlay::Message { text: "Y".into(), remaining_ms: 1 };
        assert_ne!(c, d);
    }

    #[test]
    fn with_remaining_only_touches_the_remaining_time_of_the_three_variants() {
        // The method will get its first caller only when the core publishes a
        // fresh remaining time. Without this test, a permutation of fields
        // between variants — rebuilding an `offset` from a `level` — would
        // compile and only show at integration. `Overlay`'s equality ignoring
        // `remaining_ms`, it cannot serve here: we destructure.
        let v = Overlay::Volume { level: 65, muted: true, text: "VOLUME MUET".into(), remaining_ms: 4000 };
        match v.with_remaining(7) {
            Overlay::Volume { level, muted, text, remaining_ms } => {
                assert_eq!((level, muted, text.as_str(), remaining_ms), (65, true, "VOLUME MUET", 7));
            }
            other => panic!("the variant must be preserved, got {other:?}"),
        }
        let t = Overlay::Tens { offset: 20, text: "+20".into(), remaining_ms: 4000 };
        match t.with_remaining(8) {
            Overlay::Tens { offset, text, remaining_ms } => {
                assert_eq!((offset, text.as_str(), remaining_ms), (20, "+20", 8));
            }
            other => panic!("the variant must be preserved, got {other:?}"),
        }
        let m = Overlay::Message { text: "PRESELECTION VIDE".into(), remaining_ms: 4000 };
        match m.with_remaining(9) {
            Overlay::Message { text, remaining_ms } => {
                assert_eq!((text.as_str(), remaining_ms), ("PRESELECTION VIDE", 9));
            }
            other => panic!("the variant must be preserved, got {other:?}"),
        }
    }

    #[test]
    fn the_two_new_fields_are_absent_from_json_when_empty() {
        // The SPA's payload must not fill up with nulls.
        let json = serde_json::to_string(&PlayerState::default()).unwrap();
        assert!(!json.contains("status"));
        assert!(!json.contains("overlay"));
    }

    #[test]
    fn playerstate_deserializes_the_flattened_track_and_an_overlay() {
        // This is the real path of the displays: `run_display_plugin` reads a
        // `DisplayFrame`, whose `data` for a state frame is **exactly** this
        // shape (adjacent tagging, see `display.rs`) — so this test remains
        // that of the content crossing the socket. `#[serde(flatten)]` on the
        // track combined with an internally-tagged enum (`Overlay`, `kind`) is
        // the conjunction most likely to surprise with serde. The other tests
        // of this file only cover one or the other separately; in case of a
        // regression here, the symptom would be silent on the user's side (a
        // `warn!` in the logs and a frozen screen).
        let json = r#"{
            "source": "radio",
            "volume": 65,
            "muted": false,
            "standby": false,
            "preset": 3,
            "preset_count": 12,
            "preset_name": "France Inter",
            "status": "RADIO",
            "overlay": {"kind": "volume", "level": 65, "muted": false, "text": "VOLUME 65 %", "remaining_ms": 4000},
            "artist": "Miles Davis",
            "title": "So What",
            "album": "Kind of Blue",
            "duration_s": 545,
            "origin": "icy"
        }"#;
        let state: PlayerState = serde_json::from_str(json).unwrap();
        assert_eq!(state.source, "radio");
        assert_eq!(state.preset_name.as_deref(), Some("France Inter"));
        assert_eq!(
            state.overlay,
            Some(Overlay::Volume { level: 65, muted: false, text: "VOLUME 65 %".into(), remaining_ms: 4000 })
        );
        // The flattened track: these fields come from the same JSON level as
        // `source`/`preset`/`overlay`, not from a nested object.
        assert_eq!(state.track.artist.as_deref(), Some("Miles Davis"));
        assert_eq!(state.track.title.as_deref(), Some("So What"));
        assert_eq!(state.track.album.as_deref(), Some("Kind of Blue"));
        assert_eq!(state.track.duration_s, Some(545));
        assert_eq!(state.track.origin.as_deref(), Some("icy"));
    }

    #[test]
    fn player_state_serializes_position_and_seekable_when_they_say_something() {
        let state = PlayerState {
            source: "cd".into(),
            position_s: Some(87),
            seekable: true,
            ..Default::default()
        };
        let json = serde_json::to_string(&state).unwrap();
        assert!(json.contains(r#""position_s":87"#), "{json}");
        assert!(json.contains(r#""seekable":true"#), "{json}");
    }

    /// Additive: a frame silent on these two fields stays byte-for-byte
    /// identical to what it was before this work, and a frame written by an
    /// earlier binary reads back without them.
    #[test]
    fn player_state_omits_position_and_seekable_when_they_say_nothing() {
        let state = PlayerState { source: "radio".into(), ..Default::default() };
        let json = serde_json::to_string(&state).unwrap();
        assert!(!json.contains("position_s"), "{json}");
        assert!(!json.contains("seekable"), "{json}");
        let old = r#"{"source":"radio","volume":50,"muted":false,"standby":false,"preset":null,"preset_count":null,"preset_name":null}"#;
        let reread: PlayerState = serde_json::from_str(old).unwrap();
        assert_eq!(reread.position_s, None);
        assert!(!reread.seekable);
    }

    #[test]
    fn player_state_serializes_year_and_links_when_they_say_something() {
        let state = PlayerState {
            source: "cd".into(),
            track: Track {
                year: Some(1959),
                links: vec![Link::Youtube { url: "https://www.youtube.com/watch?v=a".into() }],
                ..Default::default()
            },
            ..Default::default()
        };
        let json = serde_json::to_string(&state).unwrap();
        assert!(json.contains(r#""year":1959"#), "{json}");
        // The `Link` is internally tagged: the platform is a key of the object,
        // not one more nested object.
        assert!(
            json.contains(r#""links":[{"platform":"youtube","url":"https://www.youtube.com/watch?v=a"}]"#),
            "{json}"
        );
    }

    /// Additive: a frame silent on these two fields stays byte-for-byte
    /// identical to what it was before this work — neither `"year":null` nor
    /// `"links":[]` — and a frame written by an earlier binary reads back
    /// without them.
    #[test]
    fn player_state_omits_year_and_links_when_they_say_nothing() {
        let state = PlayerState { source: "radio".into(), ..Default::default() };
        let json = serde_json::to_string(&state).unwrap();
        assert!(!json.contains("year"), "{json}");
        assert!(!json.contains("links"), "{json}");
        let old = r#"{"source":"radio","volume":50,"muted":false,"standby":false,"preset":null,"preset_count":null,"preset_name":null,"artist":null,"title":null,"album":null,"duration_s":null,"origin":null}"#;
        let reread: PlayerState = serde_json::from_str(old).unwrap();
        assert_eq!(reread.track.year, None);
        assert!(reread.track.links.is_empty());
    }

    #[test]
    fn playback_does_not_travel_when_stopped() {
        // The additive idiom: the default value is absent from the JSON, so
        // the frames from before this field are byte-identical.
        let state = PlayerState::default();
        let json = serde_json::to_string(&state).unwrap();
        assert!(!json.contains("playback"), "playback should not be serialized: {json}");
    }

    #[test]
    fn playback_travels_in_lowercase_when_it_says_something() {
        for (p, expected) in
            [(Playback::Playing, "\"playback\":\"playing\""), (Playback::Paused, "\"playback\":\"paused\"")]
        {
            let state = PlayerState { playback: p, ..Default::default() };
            let json = serde_json::to_string(&state).unwrap();
            assert!(json.contains(expected), "{expected} absent from {json}");
            let back: PlayerState = serde_json::from_str(&json).unwrap();
            assert_eq!(back.playback, p);
        }
    }

    #[test]
    fn a_frame_without_playback_reads_back_as_stopped() {
        // Backward compatibility: a frame written before this field.
        let state: PlayerState = serde_json::from_str(
            r#"{"source":"radio","volume":40,"muted":false,"standby":false,"preset":null,"preset_count":null,"preset_name":null}"#,
        )
        .unwrap();
        assert_eq!(state.playback, Playback::Stopped);
    }

    #[test]
    fn enrichment_carries_a_position() {
        let e = Enrichment {
            identity: json!({"kind": "stream"}),
            position_s: Some(42),
            ..Default::default()
        };
        let back: Enrichment = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(back.position_s, Some(42));
        let without = r#"{"identity":{"kind":"stream"}}"#;
        assert_eq!(serde_json::from_str::<Enrichment>(without).unwrap().position_s, None);
    }

    #[test]
    fn known_makes_a_round_trip_and_reads_back_when_absent() {
        let np = NowPlaying {
            source: "files".into(),
            identity: Some(json!({"kind": "file", "path": "/mnt/nas/a.flac"})),
            known: Known {
                artist: Some("Lou Reed".into()),
                title: Some("Oooh Baby".into()),
                album: None,
                duration_s: Some(218),
                year: Some(1972),
                cover: true,
                // Non-default value, like the neighbouring fields: this test
                // checks a complete round trip, and a default `None` would not
                // have distinguished anything from a field forgotten in the
                // implementation.
                stream_title: Some("Lou Reed - Oooh Baby".into()),
            },
        };
        let back: NowPlaying = serde_json::from_str(&serde_json::to_string(&np).unwrap()).unwrap();
        assert_eq!(back, np);

        // A frame written by an earlier binary has no `known`: it must read
        // back, otherwise the overhaul cannot be deployed plugin by plugin.
        let old = r#"{"source":"radio","identity":{"kind":"stream"}}"#;
        let reread: NowPlaying = serde_json::from_str(old).unwrap();
        assert_eq!(reread.known, Known::default());
        assert!(!reread.known.cover);
    }

    #[test]
    fn empty_known_stays_silent_at_serialization() {
        // Hard constraint of this work: a frame that says nothing known must
        // stay byte-for-byte identical to what it was before this field was
        // added, otherwise every frame would grow for nothing.
        let silent = NowPlaying { source: "radio".into(), identity: None, known: Known::default() };
        let json = serde_json::to_string(&silent).unwrap();
        assert!(!json.contains("known"), "{json}");

        let talkative = NowPlaying {
            source: "files".into(),
            identity: None,
            known: Known { artist: Some("Lou Reed".into()), ..Default::default() },
        };
        let json = serde_json::to_string(&talkative).unwrap();
        assert!(json.contains("known"), "{json}");
        let back: NowPlaying = serde_json::from_str(&json).unwrap();
        assert_eq!(back, talkative);
    }

    #[test]
    fn cover_ref_has_two_distinct_forms() {
        let url = CoverRef::Url { url: "https://coverartarchive.org/release/x/front-500".into() };
        let json = serde_json::to_string(&url).unwrap();
        assert!(json.contains(r#""kind":"url""#), "{json}");
        assert_eq!(serde_json::from_str::<CoverRef>(&json).unwrap(), url);

        let path = CoverRef::Path { path: "/mnt/nas/Album/folder.jpg".into() };
        let json = serde_json::to_string(&path).unwrap();
        assert!(json.contains(r#""kind":"path""#), "{json}");
        assert_eq!(serde_json::from_str::<CoverRef>(&json).unwrap(), path);
    }

    #[test]
    fn cleaned_refuses_a_url_that_is_not_https_to_a_host() {
        // These values come from the network: the `coverUrl` field of OUI FM's
        // SSE frame is written by a third party, and it is the core that would
        // go and fetch it. Without this filter, a hostile frame makes the
        // device emit a request to the address of its choice on the local
        // network.
        for bad in [
            "http://example.org/a.jpg",
            "https://192.168.1.1/admin",
            "https://[::1]/a.jpg",
            "file:///etc/shadow",
            "ftp://example.org/a.jpg",
            "not a url",
            "",
            // Userinfo confusion: everything before the `@` is a user name, not
            // the host — a browser would indeed go to evil.example.
            "https://user@evil.example/a.jpg",
            "https://",
            "https://localhost/a.jpg",
        ] {
            let e = Enrichment {
                identity: json!(1),
                cover: Some(CoverRef::Url { url: bad.into() }),
                ..Default::default()
            }
            .cleaned();
            assert!(e.cover.is_none(), "wrongly accepted: {bad:?}");
        }
        let good = Enrichment {
            identity: json!(1),
            cover: Some(CoverRef::Url { url: " https://coverartarchive.org/x/front-500 ".into() }),
            ..Default::default()
        }
        .cleaned();
        assert_eq!(
            good.cover,
            Some(CoverRef::Url { url: "https://coverartarchive.org/x/front-500".into() })
        );
    }

    #[test]
    fn cleaned_refuses_a_relative_path_or_one_without_image_extension() {
        for bad in ["relative/folder.jpg", "/mnt/nas/notes.txt", "/mnt/nas/folder", ""] {
            let e = Enrichment {
                identity: json!(1),
                cover: Some(CoverRef::Path { path: bad.into() }),
                ..Default::default()
            }
            .cleaned();
            assert!(e.cover.is_none(), "wrongly accepted: {bad:?}");
        }
        for good in ["/mnt/nas/Album/folder.jpg", "/mnt/nas/A/Cover.JPEG", "/x/front.webp"] {
            let e = Enrichment {
                identity: json!(1),
                cover: Some(CoverRef::Path { path: good.into() }),
                ..Default::default()
            }
            .cleaned();
            assert!(e.cover.is_some(), "wrongly refused: {good:?}");
        }
    }

    #[test]
    fn a_cover_alone_remains_a_non_answer_for_the_text() {
        // Same convention as `duration_s`: a cover alone must not win the text
        // arbitration.
        let e = Enrichment {
            identity: json!(1),
            cover: Some(CoverRef::Url { url: "https://coverartarchive.org/x/front-500".into() }),
            ..Default::default()
        };
        assert!(e.is_empty());
    }

    #[test]
    fn an_enrichment_without_a_thumb_deserializes_as_before() {
        // **Backward compatibility, proved on the old shape**: a frame from a
        // plugin that has not been recompiled must still read.
        //
        // The production change that would make this fail is the new field
        // ceasing to be an `Option` — a bare `CoverRef`, or a wrapper serde
        // cannot supply — at which point every such frame is refused.
        // **Not** the loss of `#[serde(default)]`: serde fills an absent
        // `Option<T>` with `None` with or without the attribute, so naming
        // that mutation here would be naming one this assertion cannot
        // catch. The attribute earns its place next to
        // `skip_serializing_if` — as documentation of intent and as what
        // keeps the pair of them symmetrical with `cover` — not as this
        // test's subject.
        let old = r#"{"identity":1,"cover":{"kind":"url","url":"https://example.org/a.jpg"}}"#;
        let e: Enrichment = serde_json::from_str(old).expect("the old shape must still parse");
        assert!(e.cover.is_some());
        assert!(e.cover_thumb.is_none());
    }

    #[test]
    fn an_absent_thumb_does_not_grow_the_frame() {
        // The same contract as `year` and `links`: absence stays absence on
        // the wire. The production change that would make this fail: dropping
        // `skip_serializing_if` on the new field, which would stamp a
        // `"cover_thumb":null` onto every enrichment ever emitted.
        let e = Enrichment { identity: json!(1), ..Default::default() };
        assert!(!serde_json::to_string(&e).unwrap().contains("cover_thumb"));
    }

    #[test]
    fn a_thumb_is_validated_like_a_cover() {
        // The same door in, the same distrust: these values come from another
        // process. A literal IP address and a plain `http://` are refused on
        // the thumbnail side just as on the full-size side.
        let mut e = Enrichment {
            identity: json!(1),
            cover: Some(CoverRef::Url { url: "https://example.org/full.jpg".into() }),
            cover_thumb: Some(CoverRef::Url { url: "http://192.168.1.15/thumb.jpg".into() }),
            ..Default::default()
        };
        e = e.cleaned();
        assert!(e.cover.is_some(), "the full cover was well formed");
        assert!(e.cover_thumb.is_none(), "a plain-http literal IP must be dropped");
    }

    #[test]
    fn a_thumb_survives_cleaning_when_it_is_well_formed() {
        let e = Enrichment {
            identity: json!(1),
            cover: Some(CoverRef::Url {
                url: "https://coverartarchive.org/release/x/front".into(),
            }),
            cover_thumb: Some(CoverRef::Url {
                url: " https://coverartarchive.org/release/x/front-500 ".into(),
            }),
            ..Default::default()
        }
        .cleaned();
        assert_eq!(
            e.cover_thumb,
            Some(CoverRef::Url { url: "https://coverartarchive.org/release/x/front-500".into() }),
            "trimmed, like the full one"
        );
    }

    #[test]
    fn fill_only_round_trips_and_defaults_to_false() {
        // The default is "overwrite": it is the project's current rule, and it
        // is what avoids touching the three shipped plugins.
        let without: Enrichment = serde_json::from_str(r#"{"identity":{"k":1}}"#).unwrap();
        assert!(!without.fill_only);
        let e = Enrichment { identity: json!(1), fill_only: true, ..Default::default() };
        let json = serde_json::to_string(&e).unwrap();
        assert!(json.contains(r#""fill_only":true"#), "{json}");
        assert!(serde_json::from_str::<Enrichment>(&json).unwrap().fill_only);
        // Silent when false: the frame of a plugin that overwrites does not
        // grow.
        let default = Enrichment { identity: json!(1), ..Default::default() };
        assert!(!serde_json::to_string(&default).unwrap().contains("fill_only"));
    }

    #[test]
    fn absent_stream_title_does_not_grow_the_frame() {
        // Same contract as `covers` and `known`: a new field must change
        // nothing in the most common frame, otherwise every per-second
        // playback frame pays for the addition.
        let json = serde_json::to_string(&Known::default()).unwrap();
        assert!(!json.contains("stream_title"), "{json}");
    }

    #[test]
    fn stream_title_travels_when_present() {
        let k = Known { stream_title: Some("Miles Davis - So What".into()), ..Default::default() };
        let json = serde_json::to_string(&k).unwrap();
        assert!(json.contains(r#""stream_title":"Miles Davis - So What""#), "{json}");
        assert_eq!(serde_json::from_str::<Known>(&json).unwrap(), k);
    }

    #[test]
    fn a_frame_from_an_earlier_binary_reads_back() {
        let k: Known = serde_json::from_str(r#"{"title":"X"}"#).unwrap();
        assert_eq!(k.stream_title, None);
    }

    #[test]
    fn track_omits_the_cover_when_there_is_none() {
        let json = serde_json::to_string(&PlayerState::default()).unwrap();
        assert!(!json.contains("cover_href"), "{json}");
        assert!(!json.contains("cover_origin"), "{json}");

        let state = PlayerState {
            source: "files".into(),
            track: Track {
                cover_href: Some("/api/cover/1a2b3c".into()),
                cover_origin: Some("files".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let json = serde_json::to_string(&state).unwrap();
        assert!(json.contains(r#""cover_href":"/api/cover/1a2b3c""#), "{json}");
        assert!(json.contains(r#""cover_origin":"files""#), "{json}");
    }
}
