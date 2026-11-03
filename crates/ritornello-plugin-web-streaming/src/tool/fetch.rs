//! Downloads with a deadline and a size cap, nothing else.
//!
//! What is fetched is never trusted for being fetched: yt-dlp is checked
//! against its signature, deno against its published sum. The fetcher only
//! guarantees that a slow or endless answer cannot hold the plugin, and that
//! nothing leaves https (a redirect included).

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

#[derive(Clone, Copy, Debug)]
pub struct Limit {
    pub max_bytes: usize,
    /// For the whole exchange, connection to last byte.
    pub deadline: Duration,
}

pub trait Fetch: Send + Sync {
    fn get(&self, url: &str, limit: Limit) -> BoxFuture<Result<Vec<u8>, String>>;

    /// Where `url` redirects to (its `Location`), without following it.
    fn redirect_of(&self, url: &str, deadline: Duration) -> BoxFuture<Result<String, String>>;
}

/// The real thing: rustls, https only, a few redirects (GitHub's "latest"
/// addresses redirect twice, to the release and then to its storage).
pub struct Https {
    pub connect: Duration,
}

impl Default for Https {
    fn default() -> Self {
        Self { connect: Duration::from_secs(15) }
    }
}

impl Fetch for Https {
    fn get(&self, url: &str, limit: Limit) -> BoxFuture<Result<Vec<u8>, String>> {
        let url = url.to_string();
        let connect = self.connect;
        Box::pin(async move {
            let run = async {
                let client = reqwest::Client::builder()
                    .connect_timeout(connect)
                    .timeout(limit.deadline)
                    .https_only(true)
                    .redirect(reqwest::redirect::Policy::limited(5))
                    .user_agent(concat!("ritornello-plugin-web-streaming/", env!("CARGO_PKG_VERSION")))
                    .build()
                    .map_err(|e| e.to_string())?;
                let mut response = client
                    .get(&url)
                    .send()
                    .await
                    .and_then(|r| r.error_for_status())
                    .map_err(|e| e.to_string())?;
                if response.content_length().is_some_and(|n| n > limit.max_bytes as u64) {
                    return Err(format!("{url}: larger than {} bytes", limit.max_bytes));
                }
                let mut body = Vec::new();
                while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
                    if body.len() + chunk.len() > limit.max_bytes {
                        return Err(format!("{url}: larger than {} bytes", limit.max_bytes));
                    }
                    body.extend_from_slice(&chunk);
                }
                Ok(body)
            };
            // `reqwest`'s own timeout already covers the exchange; this one
            // also covers the client's construction and is the documented
            // bound.
            tokio::time::timeout(limit.deadline, run).await.unwrap_or_else(|_| Err(format!("{url}: timed out")))
        })
    }

    fn redirect_of(&self, url: &str, deadline: Duration) -> BoxFuture<Result<String, String>> {
        let url = url.to_string();
        let connect = self.connect;
        Box::pin(async move {
            let run = async {
                let client = reqwest::Client::builder()
                    .connect_timeout(connect)
                    .timeout(deadline)
                    .https_only(true)
                    .redirect(reqwest::redirect::Policy::none())
                    .user_agent(concat!("ritornello-plugin-web-streaming/", env!("CARGO_PKG_VERSION")))
                    .build()
                    .map_err(|e| e.to_string())?;
                let response = client.get(&url).send().await.map_err(|e| e.to_string())?;
                if !response.status().is_redirection() {
                    return Err(format!("{url}: {} instead of a redirect", response.status()));
                }
                response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .map(str::to_string)
                    .ok_or_else(|| format!("{url}: a redirect without a location"))
            };
            tokio::time::timeout(deadline, run).await.unwrap_or_else(|_| Err(format!("{url}: timed out")))
        })
    }
}
