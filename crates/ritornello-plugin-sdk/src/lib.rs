pub mod args;
pub mod client;
pub mod runtime;
pub mod server;

pub use args::{admin_socket, socket_kind, plugin_name, register_socket, socket_prefix};
pub use client::{
    run_input_client, run_metadata_client, AdminClient, AdminIpcError, DisplayClient, SourceClient,
    SourceUpdate,
};
pub use runtime::Runtime;
pub use server::{
    bind_admin, bind_display, bind_input, bind_metadata, bind_source, run_admin_plugin,
    run_display_plugin, run_input_plugin, run_metadata_plugin, run_source_plugin, serve_admin,
    serve_display, serve_input, serve_metadata, serve_source, AdminPlugin, DisplayPlugin,
    InputPlugin, MetadataPlugin, Notification, SourceOutcome, SourcePlugin,
};
