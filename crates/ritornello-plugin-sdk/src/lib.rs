pub mod client;
pub mod server;

pub use client::{run_input_client, AdminClient, DisplayClient, SourceClient};
pub use server::{
    run_admin_plugin, run_display_plugin, run_input_plugin, run_source_plugin, AdminPlugin,
    DisplayPlugin, InputPlugin, SourceOutcome, SourcePlugin,
};
