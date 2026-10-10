/// The **bootstrap number** of the announcement.
///
/// It answers one question only: *can this binary even read the other's
/// announcement?* The announcement carries the version of every wire contract
/// (`Contract`, `ContractVersion`) and therefore cannot version itself with
/// them: this number is the one thing left that is compared by equality,
/// before anything else is looked at. It is deliberately not the version of
/// the product, which lives in `[workspace.package]` and moves on every
/// release.
///
/// It moved from 1 to 2 when contract versions were introduced, so that a core
/// older than them refuses every newer plugin (and the reverse) instead of
/// reading an announcement without `protocol` as 1 and accepting it silently.
///
/// It moves again only if the announcement's own format breaks, **or when a
/// contract is added**: a new contract comes with a new kind or a new socket,
/// and the announcement's closed enums (`PluginKind`, `Contract`) make an
/// announcement naming it unreadable to an older core, so the two sides must
/// be republished together. (Such an older core cannot even read `protocol`
/// from it: it logs the announcement as unreadable and shows the plugin as
/// silent, not as incompatible.) Either move republishes every component that links this crate
/// (`scripts/changed-components.sh` refuses the release otherwise), and the
/// core shows a plugin announcing another number as "incompatible". There is
/// no backward compatibility to maintain: breaks stay free, they are only
/// signalled.
///
/// **What forces the decision** is `tests/wire_fingerprint.rs`: it serializes
/// a sample of every wire message and compares it with a committed fixture,
/// so the wire cannot change without someone choosing, in that test's own
/// words, between "break: bump this number" and "compatible: update the
/// fingerprint and say why".
pub const PROTOCOL_VERSION: u32 = 2;

pub mod admin;
pub mod command;
pub mod contract;
pub mod display;
pub mod metadata;
pub mod register;
pub mod source;

pub use admin::{AdminReq, AdminRequest, AdminResponse, AdminResult};
pub use command::{Command, InputMessage, Repeat};
pub use contract::{
    Contract, ContractVersion, ADMIN_CONTRACT, DISPLAY_CONTRACT, INPUT_CONTRACT, METADATA_CONTRACT,
    SOURCE_CONTRACT,
};
pub use display::{SourcesCatalog, Cover, DisplayFrame, SourceCatalog, COVER_MAX_BYTES};
pub use metadata::{
    valid_year, CoverRef, Enrichment, DateFormat, Clock, IdentityUpdate, Known, Link, Track,
    NowPlaying, Overlay, Playback, PlayerState, Provenance,
};
pub use register::{Announcement, PluginKind, ANNOUNCEMENT_MAX_BYTES};
pub use source::{Armed, PlayRequest, Preset, SourceAction, SourceMessage, SourceReq, SourceRequest, Text};
