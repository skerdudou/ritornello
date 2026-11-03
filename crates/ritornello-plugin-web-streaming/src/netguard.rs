//! Never reaching the local network, on the plugin's own side.
//!
//! The plugin fetches one thing itself: the cover a probe named, an address
//! that comes from remote data. It must never become a way to reach the
//! operator's own network (the router's page, the NAS, the core's own HTTP
//! port), whichever site a link was opened from. Three gates, all needed:
//!
//! - [`url_ok`] judges the address before anything is sent: https, no
//!   credentials, the default port, and a host that, when it is an IP
//!   literal, is not local. reqwest connects to a literal **without calling
//!   any resolver**, so this is the only place a literal is judged;
//! - [`PublicOnly`] is the resolver of the client: a name resolves through
//!   the system, and the local addresses of its answer are dropped (all
//!   local: an error). The connection is made to an address it returned, so
//!   a DNS that changes its answer between two lookups gains nothing;
//! - [`redirect_policy`] re-judges every hop with the same [`url_ok`] (a
//!   redirect to a literal would otherwise skip the resolver) and stops
//!   after [`MAX_HOPS`].
//!
//! [`is_local`] is the predicate, mirrored by the Python one of the wrapper
//! (`tool/wrapper.rs`): both tests read the same table,
//! `tests/fixtures/local-network.txt`.

use std::future::Future;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use url::{Host, Url};

/// An address of the local network, or one that would reach it: loopback,
/// "this network", the private ranges (10/8, 172.16/12, 192.168/16), the
/// shared CGNAT range 100.64/10, link-local, multicast, reserved and
/// broadcast; on IPv6 `::/96`, unique and link- and site-local, multicast,
/// and every form that carries an IPv4 (mapped, NAT64, 6to4) judged on that
/// IPv4. The ranges are written out, not taken from the standard library's
/// helpers, whose definitions are not this list and have moved over time.
pub fn is_local(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => v4_is_local(a),
        IpAddr::V6(a) => v6_is_local(a),
    }
}

fn v4_is_local(a: Ipv4Addr) -> bool {
    let [a, b, ..] = a.octets();
    a == 0 // 0.0.0.0/8
        || a == 10 // 10/8
        || (a == 100 && b & 0xC0 == 64) // 100.64/10
        || a == 127 // 127/8
        || (a == 169 && b == 254) // 169.254/16
        || (a == 172 && b & 0xF0 == 16) // 172.16/12
        || (a == 192 && b == 168) // 192.168/16
        || a >= 224 // 224/4 multicast, 240/4 reserved and broadcast
}

/// The IPv4 held in two 16-bit segments.
fn v4_of(high: u16, low: u16) -> Ipv4Addr {
    Ipv4Addr::from((u32::from(high) << 16) | u32::from(low))
}

fn v6_is_local(a: Ipv6Addr) -> bool {
    let s = a.segments();
    // ::ffff:0:0/96, IPv4-mapped.
    if s[..6] == [0, 0, 0, 0, 0, 0xFFFF] {
        return v4_is_local(v4_of(s[6], s[7]));
    }
    // 64:ff9b::/96, NAT64.
    if s[..6] == [0x64, 0xFF9B, 0, 0, 0, 0] {
        return v4_is_local(v4_of(s[6], s[7]));
    }
    // 2002::/16, 6to4.
    if s[0] == 0x2002 {
        return v4_is_local(v4_of(s[1], s[2]));
    }
    s[..6] == [0; 6] // ::/96
        || (s[0] == 0x64 && s[1] == 0xFF9B && s[2] == 1) // 64:ff9b:1::/48
        || s[0] & 0xFE00 == 0xFC00 // fc00::/7
        || s[0] & 0xFFC0 == 0xFE80 // fe80::/10
        || s[0] & 0xFFC0 == 0xFEC0 // fec0::/10
        || s[0] & 0xFF00 == 0xFF00 // ff00::/8
}

/// An address the plugin may fetch from: https, no credentials, the default
/// port, a host, and that host not a local IP literal (a name is judged when
/// it resolves, by [`PublicOnly`]).
pub fn url_ok(u: &Url) -> bool {
    u.scheme() == "https"
        && u.port().is_none()
        && u.username().is_empty()
        && u.password().is_none()
        && match u.host() {
            Some(Host::Domain(_)) => true,
            Some(Host::Ipv4(a)) => !is_local(IpAddr::V4(a)),
            Some(Host::Ipv6(a)) => !is_local(IpAddr::V6(a)),
            None => false,
        }
}

/// What a name resolves to, before any judging: the system's resolver in
/// production, a fixed answer in the tests.
pub type Lookup = Arc<dyn Fn(String) -> Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send>> + Send + Sync>;

/// The system's resolver (`getaddrinfo`, as reqwest's own default).
pub fn system_lookup() -> Lookup {
    Arc::new(|name| Box::pin(async move { Ok(tokio::net::lookup_host((name.as_str(), 0)).await?.collect()) }))
}

/// The addresses of an answer that are not local, in their order.
pub fn public_addrs(addrs: Vec<SocketAddr>) -> Vec<SocketAddr> {
    addrs.into_iter().filter(|a| !is_local(a.ip())).collect()
}

/// reqwest's resolver: [`Lookup`]'s answer without its local addresses, and
/// an error when nothing else is left.
pub struct PublicOnly(pub Lookup);

impl Resolve for PublicOnly {
    fn resolve(&self, name: Name) -> Resolving {
        let name = name.as_str().to_string();
        let answer = (self.0)(name.clone());
        Box::pin(async move {
            let addrs = public_addrs(answer.await?);
            if addrs.is_empty() {
                return Err(format!("{name} resolves only to the local network").into());
            }
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

/// The most redirects one fetch follows.
pub const MAX_HOPS: usize = 5;

/// Whether the redirect to `next` may be followed, `previous` being the
/// addresses already visited (the first one included, as reqwest counts).
pub fn hop_ok(next: &Url, previous: usize, url_ok: &(dyn Fn(&Url) -> bool + Send + Sync)) -> Result<(), String> {
    if previous > MAX_HOPS {
        return Err(format!("more than {MAX_HOPS} redirects"));
    }
    if !url_ok(next) {
        return Err(format!("redirect to a refused address: {next}"));
    }
    Ok(())
}

/// A judge of addresses: [`url_ok`] in production.
pub type UrlCheck = Arc<dyn Fn(&Url) -> bool + Send + Sync>;

pub fn redirect_policy(url_ok: UrlCheck) -> reqwest::redirect::Policy {
    reqwest::redirect::Policy::custom(move |attempt| match hop_ok(attempt.url(), attempt.previous().len(), &*url_ok) {
        Ok(()) => attempt.follow(),
        Err(e) => attempt.error(e),
    })
}

/// How a fetch is guarded: the judge of each address (the first one and
/// every hop) and what names resolve with.
#[derive(Clone)]
pub struct Guard {
    pub url_ok: UrlCheck,
    pub lookup: Lookup,
}

impl Guard {
    /// The production guard: [`url_ok`] and the system's resolver.
    pub fn system() -> Self {
        Self { url_ok: Arc::new(url_ok), lookup: system_lookup() }
    }

    /// A client that only reaches what the guard lets through. No proxy: a
    /// proxy would resolve the name itself, out of the resolver's sight.
    pub fn client(&self, connect: Duration, total: Duration) -> reqwest::Result<reqwest::Client> {
        reqwest::Client::builder()
            .connect_timeout(connect)
            .timeout(total)
            .no_proxy()
            .dns_resolver(Arc::new(PublicOnly(self.lookup.clone())))
            .redirect(redirect_policy(self.url_ok.clone()))
            .build()
    }
}

#[cfg(test)]
pub(crate) mod tests;
