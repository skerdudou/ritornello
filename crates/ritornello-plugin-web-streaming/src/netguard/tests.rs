use super::*;
use std::str::FromStr;

/// The table both predicates are held to (the Python one's test reads it
/// too): `(address, local)`, comments and blank lines skipped.
pub(crate) fn table() -> Vec<(String, bool)> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/local-network.txt");
    std::fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|l| l.split('#').next().unwrap().trim())
        .filter(|l| !l.is_empty())
        .map(|l| {
            let (ip, verdict) = l.split_once(' ').unwrap();
            let local = match verdict.trim() {
                "local" => true,
                "public" => false,
                other => panic!("{other:?} in {l:?}"),
            };
            (ip.to_string(), local)
        })
        .collect()
}

#[test]
fn the_predicate_agrees_with_the_shared_table_line_by_line() {
    let table = table();
    assert!(table.len() > 60, "the table was read: {}", table.len());
    let mut wrong = Vec::new();
    for (ip, local) in &table {
        match IpAddr::from_str(ip) {
            Ok(addr) => {
                if is_local(addr) != *local {
                    wrong.push(format!("{ip}: expected {}", if *local { "local" } else { "public" }));
                }
            }
            // What Rust cannot parse is never a host of a URL as an IP
            // literal; the Python predicate refuses it, and the table says so.
            Err(_) => assert!(local, "{ip} is unparseable, so the table must call it local"),
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn a_cover_address_is_judged_before_anything_is_sent() {
    let ok = |s: &str| url_ok(&Url::parse(s).unwrap());
    for good in [
        "https://i.ytimg.com/vi/x/hq.jpg",
        "https://f4.bcbits.com/img/a1_10.jpg",
        "https://8.8.8.8/x.jpg",
        "https://[2001:4860:4860::8888]/x.jpg",
        // A name that resolves locally passes here: the resolver drops it.
        "https://localhost/x.jpg",
    ] {
        assert!(ok(good), "{good}");
    }
    for bad in [
        "http://i.ytimg.com/x.jpg",
        "ftp://i.ytimg.com/x.jpg",
        "https://user@i.ytimg.com/x.jpg",
        "https://user:pw@i.ytimg.com/x.jpg",
        "https://:pw@i.ytimg.com/x.jpg",
        "https://i.ytimg.com:8443/x.jpg",
        "https://127.0.0.1/x.jpg",
        "https://192.168.1.10/x.jpg",
        "https://10.0.0.1/x.jpg",
        "https://100.64.0.1/x.jpg",
        // Spellings the URL parser reads as a local IPv4.
        "https://0x7f.1/x.jpg",
        "https://2130706433/x.jpg",
        "https://127.1/x.jpg",
        "https://[::1]/x.jpg",
        "https://[::ffff:127.0.0.1]/x.jpg",
        "https://[::ffff:c0a8:10a]/x.jpg",
        "https://[64:ff9b::a00:1]/x.jpg",
        "https://[2002:c0a8:101::]/x.jpg",
        "https://[fe80::1]/x.jpg",
        "https://[fd00::1]/x.jpg",
    ] {
        assert!(!ok(bad), "{bad}");
    }
    assert!(!url_ok(&Url::parse("data:image/jpeg;base64,AA").unwrap()), "no host");
}

fn fixed(addrs: &[&str]) -> Lookup {
    let addrs: Vec<SocketAddr> = addrs.iter().map(|a| SocketAddr::new(a.parse().unwrap(), 0)).collect();
    Arc::new(move |_| {
        let addrs = addrs.clone();
        Box::pin(async move { Ok(addrs) })
    })
}

async fn resolve(lookup: Lookup, name: &str) -> Result<Vec<IpAddr>, String> {
    let resolver = PublicOnly(lookup);
    match resolver.resolve(Name::from_str(name).unwrap()).await {
        Ok(addrs) => Ok(addrs.map(|a| a.ip()).collect()),
        Err(e) => Err(e.to_string()),
    }
}

#[tokio::test]
async fn the_resolver_drops_the_local_addresses_of_an_answer() {
    let mixed = fixed(&["192.168.1.10", "8.8.8.8", "::1", "2001:4860:4860::8888", "::ffff:10.0.0.1"]);
    assert_eq!(
        resolve(mixed, "rebind.example").await,
        Ok(vec!["8.8.8.8".parse().unwrap(), "2001:4860:4860::8888".parse().unwrap()])
    );
    let local = fixed(&["127.0.0.1", "::1", "64:ff9b::c0a8:101"]);
    let err = resolve(local, "rebind.example").await.unwrap_err();
    assert!(err.contains("rebind.example resolves only to the local network"), "{err}");
    // The inner lookup's own failure passes as a failure.
    let failing: Lookup = Arc::new(|_| Box::pin(async { Err(io::Error::other("no such name")) }));
    assert!(resolve(failing, "x.example").await.is_err());
}

#[test]
fn every_hop_is_judged_and_the_chain_is_capped() {
    let judge = |s: &str, previous| hop_ok(&Url::parse(s).unwrap(), previous, &url_ok);
    assert_eq!(judge("https://i.ytimg.com/x.jpg", 1), Ok(()));
    assert!(judge("http://127.0.0.1/", 1).is_err(), "http and a local literal");
    assert!(judge("https://10.0.0.1/", 1).is_err(), "a local literal, over https");
    assert!(judge("https://[::ffff:192.168.0.1]/", 1).is_err());
    assert!(judge("http://i.ytimg.com/x.jpg", 1).is_err(), "down to http");
    assert_eq!(judge("https://i.ytimg.com/x.jpg", MAX_HOPS), Ok(()), "the fifth redirect");
    assert!(judge("https://i.ytimg.com/x.jpg", MAX_HOPS + 1).is_err(), "the sixth");
}
