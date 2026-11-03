mod admin;
mod source;

use anyhow::Result;
use ritornello_plugin_web_streaming::download::{Ctx, Limits};
use ritornello_plugin_web_streaming::entries::{Entries, ListCache};
use ritornello_plugin_web_streaming::jobs::{self, Jobs};
use ritornello_plugin_web_streaming::targets::files_roots_path;
use ritornello_plugin_web_streaming::tool::{self, Tool};
use ritornello_plugin_web_streaming::ytdlp::Ytdlp;
use ritornello_plugin_web_streaming::WEB_STREAMING_EN;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock as AsyncRwLock;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt().with_target(false).init();
    let data = ritornello_plugin_sdk::data_dir();
    let entries = Entries::load(&data.join("entries.toml")).unwrap_or_else(|e| {
        tracing::warn!("entries.toml unreadable ({e:#}): starting without presets");
        Entries::default()
    });
    let entries = Arc::new(AsyncRwLock::new(entries));
    // The download queue first: yt-dlp's updater takes its gate, so an update
    // waits for the job in progress instead of swapping the tool under it.
    let ctx = Ctx {
        ytdlp: Arc::new(Ytdlp::new(&data)),
        lists: ListCache::new(&data),
        limits: Limits::default(),
    };
    let resolver = jobs::resolver_for(files_roots_path(&data), Duration::from_secs(10));
    let jobs = Jobs::start(&data, entries.clone(), ctx, resolver);
    // yt-dlp: installed at once when absent, then checked daily, in the
    // background.
    let tool = Tool::new(&data, jobs.tool_gate())?;
    tokio::spawn(tool::schedule(tool.clone()));
    // Ticked by the admin half after each save; the source half announces the
    // new table to the core.
    let (saved_tx, saved_rx) = tokio::sync::watch::channel(());
    let admin = admin::WebStreamingAdmin::new(
        data.clone(),
        entries.clone(),
        tool,
        jobs,
        saved_tx,
        admin::Environment::real(&data),
    );
    ritornello_plugin_sdk::declare_runtime!()?
        .texts([("en", WEB_STREAMING_EN)])?
        .source(source::WebStreamingSource::new(data, entries, Some(saved_rx)))?
        .admin(admin)?
        .run()
        .await
}

#[cfg(test)]
mod tests {
    /// The `.texts(...)` call above hands the core exactly the catalogue
    /// the deploy locales are checked against.
    #[test]
    fn the_embedded_english_is_not_empty() {
        let en = ritornello_i18n::try_parse(super::WEB_STREAMING_EN).unwrap();
        assert!(en.contains_key("title"));
    }
}
