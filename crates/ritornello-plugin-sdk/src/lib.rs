pub mod args;
pub mod client;
pub mod server;

pub use args::{admin_socket_path, socket_path};
pub use client::{
    run_input_client, run_metadata_client, AdminClient, AdminIpcError, DisplayClient, SourceClient,
    SourceUpdate,
};
pub use server::{
    run_admin_plugin, run_display_plugin, run_input_plugin, run_metadata_plugin, run_source_plugin,
    AdminPlugin, DisplayPlugin, InputPlugin, MetadataPlugin, Notification, SourceOutcome,
    SourcePlugin,
};
