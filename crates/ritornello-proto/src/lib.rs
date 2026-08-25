pub mod admin;
pub mod command;
pub mod display;
pub mod metadata;
pub mod register;
pub mod source;

pub use admin::{AdminReq, AdminRequest, AdminResponse, AdminResult};
pub use command::{Command, InputMessage};
pub use display::{Catalogue, Cover, DisplayFrame, SourceCatalogue, COVER_MAX_BYTES};
pub use metadata::{
    CoverRef, Enrichment, IdentityUpdate, Known, Morceau, NowPlaying, Overlay, Playback,
    PlayerState,
};
pub use register::{Announcement, PluginKind};
pub use source::{Preset, SourceAction, SourceMessage, SourceReq, SourceRequest};
