pub mod client;
pub mod server;

pub use client::{run_input_client, SinkClient, SourceClient};
pub use server::{
    run_input_plugin, run_sink_plugin, run_source_plugin, InputPlugin, SinkOutcome, SinkPlugin,
    SourceOutcome, SourcePlugin,
};
