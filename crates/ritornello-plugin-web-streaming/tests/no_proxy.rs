//! The cover client never goes through a proxy: a proxy resolves the name
//! itself, out of the guard's resolver's sight, and could reach the local
//! network for us. A binary of its own because the proxy is set in the
//! environment, which every test of a binary shares.

use std::io::ErrorKind;
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use ritornello_plugin_web_streaming::download::fetch_jpg;
use ritornello_plugin_web_streaming::netguard::{self, Guard, Lookup};

#[tokio::test]
async fn the_cover_client_ignores_a_proxy_from_the_environment() {
    let proxy = TcpListener::bind("127.0.0.1:0").unwrap();
    proxy.set_nonblocking(true).unwrap();
    let address = format!("http://{}", proxy.local_addr().unwrap());
    // SAFETY: the only test of this binary, before any thread of ours reads
    // the environment.
    unsafe {
        for name in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
            std::env::set_var(name, &address);
        }
        std::env::remove_var("NO_PROXY");
        std::env::remove_var("no_proxy");
    }
    // The production judge, and a resolver that knows no name: without a
    // proxy the fetch fails at once and nothing reaches the network.
    let nothing: Lookup = Arc::new(|name| Box::pin(async move { Err(std::io::Error::other(format!("{name}: unknown"))) }));
    let guard = Guard { url_ok: Arc::new(netguard::url_ok), lookup: nothing };
    let got = fetch_jpg("https://covers.example/a.jpg", Duration::from_secs(2), Duration::from_secs(3), &guard).await;
    assert_eq!(got, None);
    assert_eq!(proxy.accept().map(|_| ()).unwrap_err().kind(), ErrorKind::WouldBlock, "the client went through the proxy");
}
