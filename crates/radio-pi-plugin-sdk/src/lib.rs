pub mod client;
pub mod server;

pub use client::{run_input_client, DisplayClient, SourceClient};
pub use server::{
    run_display_plugin, run_input_plugin, run_source_plugin, DisplayPlugin, InputPlugin,
    SourceOutcome, SourcePlugin,
};
