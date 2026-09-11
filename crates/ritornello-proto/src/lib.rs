/// Version of the wire protocol between the core and a plugin.
///
/// It answers one question only — *does this binary speak the same language as
/// that one?* — and it is deliberately not the version of the product, which
/// lives in `[workspace.package]` and moves on every release. Two numbers,
/// because a typo fixed in the core must not read as an incompatibility with
/// nine plugins.
///
/// **It moves only on a break, never on an addition.** Every field added to
/// this protocol so far (`admin`, `covers`, `ui_version`, the eject
/// capability) was absorbed by serde's defaults, and the tests that pin that
/// behaviour are the proof. So this number may well never move; that rarity is
/// exactly what gives it its meaning — when it does move, everything must be
/// replaced, and a bump of the product's minor goes with it.
pub const PROTOCOL_VERSION: u32 = 1;

pub mod admin;
pub mod command;
pub mod display;
pub mod metadata;
pub mod register;
pub mod source;

pub use admin::{AdminReq, AdminRequest, AdminResponse, AdminResult};
pub use command::{Command, InputMessage};
pub use display::{SourcesCatalog, Cover, DisplayFrame, SourceCatalog, COVER_MAX_BYTES};
pub use metadata::{
    valid_year, CoverRef, Enrichment, DateFormat, Clock, IdentityUpdate, Known, Link, Track,
    NowPlaying, Overlay, Playback, PlayerState, Provenance,
};
pub use register::{Announcement, PluginKind, ANNOUNCEMENT_MAX_BYTES};
pub use source::{Preset, SourceAction, SourceMessage, SourceReq, SourceRequest};
