//! The links an operator pastes on the admin page, one per line: recognised,
//! reduced to what matters, or refused with a reason.
//!
//! **Two families.** A link whose host belongs to YouTube (`youtube.com`,
//! `youtu.be`, `youtube-nocookie.com` and any subdomain of them, a trailing
//! dot included) follows YouTube's rules below, and only them: a YouTube link
//! those rules refuse stays refused, it never slips through as a generic one.
//! Any other `http`/`https` link is a [`Link::Web`]: whatever yt-dlp makes of
//! it is what plays.
//!
//! **YouTube** (http or https, host exactly `youtube.com`, `www.`, `m.`,
//! `music.` or `youtu.be`, no port): `/watch?v=ID`, `/playlist?list=ID`,
//! `/watch?v=ID&list=ID`, `/shorts/ID`, `/embed/ID`, and `youtu.be/ID`
//! (optionally with `?list=ID`). Refused: any other host of the family, a
//! port, `/live/ID` (a live stream never ends, so it is out of scope:
//! `LinkError::LiveStream`), `/v/ID` and every other path, extra path
//! segments after the id, a malformed id (video: exactly 11 characters of
//! `[A-Za-z0-9_-]`; playlist: one or more of them), and a repeated `v` or
//! `list` parameter (ambiguous). A malformed id refuses the whole link, never
//! just the part that is wrong.
//!
//! **Any other site**: `http` or `https`, a host, no credentials, no control
//! character or whitespace anywhere in the raw text (the `url` crate would
//! silently drop a tab or a newline, so the raw text is judged before it is
//! parsed), and a host that, when it is an IP literal, is not of the local
//! network ([`netguard::is_local`], in every spelling the URL standard reads
//! as an address: `0x7f.1`, `2130706433`...). A name is judged when it
//! resolves, by the guard of every connection (`netguard.rs` and the
//! wrapper's): this is only an early, clear refusal. The link is kept
//! normalised: scheme and host lowercased by the parser, fragment dropped.
//!
//! The same function judges the addresses yt-dlp gives for the entries of a
//! list (`ytdlp::Video::play_url`): what reaches an m3u is what an operator
//! could have pasted.
//!
//! The host is judged on the parsed URL, never on the raw string, so
//! `youtube.com.evil.example` and `evil-youtube.com` are generic links, and
//! `https://youtube.com@evil.example/` (whose real host is `evil.example`)
//! is refused for its credentials.

use crate::netguard;
use url::Host;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Video { id: String },
    Playlist { id: String },
    Both { video: String, list: String },
    /// A link of any other site, normalised. Whether it names one track or a
    /// list is yt-dlp's answer.
    Web { url: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkError {
    NotAUrl,
    /// A host of YouTube's family that YouTube's rules refuse (with its port
    /// when it has one).
    ForeignHost(String),
    NoMedia,
    /// `/live/ID`: an endless stream, not supported.
    LiveStream,
    /// An IP literal of the local network.
    LocalNetwork(String),
}

const HOSTS: [&str; 4] = ["youtube.com", "www.youtube.com", "m.youtube.com", "music.youtube.com"];

/// The domains whose links only YouTube's rules may accept.
const YOUTUBE_DOMAINS: [&str; 3] = ["youtube.com", "youtu.be", "youtube-nocookie.com"];

fn is_id_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// A video id is exactly 11 characters of `[A-Za-z0-9_-]`.
fn video_id(s: &str) -> Option<String> {
    (s.len() == 11 && s.chars().all(is_id_char)).then(|| s.to_string())
}

/// Playlist ids vary in length (PL..., OLAK5uy_..., RD...): characters only.
fn list_id(s: &str) -> Option<String> {
    (!s.is_empty() && s.chars().all(is_id_char)).then(|| s.to_string())
}

/// Whether `host` (lowercased by the parser) is YouTube's, in any spelling:
/// one of its domains or a subdomain of one, with or without a trailing dot.
fn is_youtube_family(host: &str) -> bool {
    let host = host.strip_suffix('.').unwrap_or(host);
    YOUTUBE_DOMAINS.iter().any(|d| host == *d || host.strip_suffix(d).is_some_and(|rest| rest.ends_with('.')))
}

pub fn parse_link(raw: &str) -> Result<Link, LinkError> {
    let raw = raw.trim();
    // Judged on the raw text: the parser drops tabs and newlines silently, and
    // a link that held one is not the link that would be written down.
    if raw.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(LinkError::NotAUrl);
    }
    let url = url::Url::parse(raw).map_err(|_| LinkError::NotAUrl)?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(LinkError::NotAUrl);
    }
    // `Url` lowercases the host and resolves userinfo, so this is the host the
    // request would really go to.
    let host = url.host_str().ok_or(LinkError::NotAUrl)?.to_string();
    if is_youtube_family(&host) { youtube(&url, host) } else { web(url, host) }
}

/// Any other site: see the module's comment.
fn web(mut url: url::Url, host: String) -> Result<Link, LinkError> {
    if !url.username().is_empty() || url.password().is_some() {
        return Err(LinkError::NotAUrl);
    }
    let local = match url.host() {
        Some(Host::Ipv4(a)) => netguard::is_local(a.into()),
        Some(Host::Ipv6(a)) => netguard::is_local(a.into()),
        Some(Host::Domain(_)) => false,
        None => return Err(LinkError::NotAUrl),
    };
    if local {
        return Err(LinkError::LocalNetwork(host));
    }
    url.set_fragment(None);
    Ok(Link::Web { url: url.into() })
}

/// YouTube's rules: see the module's comment.
fn youtube(url: &url::Url, host: String) -> Result<Link, LinkError> {
    if let Some(port) = url.port() {
        return Err(LinkError::ForeignHost(format!("{host}:{port}")));
    }
    // A parameter given twice is ambiguous: refuse rather than pick one.
    let query = |name: &str| -> Result<Option<String>, LinkError> {
        let mut values = url.query_pairs().filter(|(k, _)| k == name).map(|(_, v)| v.into_owned());
        let first = values.next();
        if values.next().is_some() {
            return Err(LinkError::NoMedia);
        }
        Ok(first)
    };
    let mut segments: Vec<&str> = url.path_segments().map(|s| s.collect()).unwrap_or_default();
    segments.retain(|s| !s.is_empty());

    let (video, list): (Option<String>, Option<String>) = if host == "youtu.be" {
        match segments.as_slice() {
            [id] => (Some(id.to_string()), query("list")?),
            _ => return Err(LinkError::NoMedia),
        }
    } else if HOSTS.contains(&host.as_str()) {
        match segments.as_slice() {
            ["watch"] => (query("v")?, query("list")?),
            ["playlist"] => (None, query("list")?),
            ["shorts", id] | ["embed", id] => (Some(id.to_string()), None),
            ["live", _] => return Err(LinkError::LiveStream),
            _ => return Err(LinkError::NoMedia),
        }
    } else {
        return Err(LinkError::ForeignHost(host));
    };

    // A present but malformed id refuses the whole link, rather than quietly
    // playing something other than what was pasted.
    let video = video.map(|v| video_id(&v).ok_or(LinkError::NoMedia)).transpose()?;
    let list = list.map(|l| list_id(&l).ok_or(LinkError::NoMedia)).transpose()?;
    match (video, list) {
        (Some(video), Some(list)) => Ok(Link::Both { video, list }),
        (Some(id), None) => Ok(Link::Video { id }),
        (None, Some(id)) => Ok(Link::Playlist { id }),
        (None, None) => Err(LinkError::NoMedia),
    }
}

/// The one spelling of a video's address, used wherever one is written down
/// (the stored preset, the m3u handed to mpv).
///
/// An id that is not exactly a video id (11 characters of `[A-Za-z0-9_-]`) has
/// no address: ids read back from yt-dlp or from a cache file are data from
/// outside, and one holding a newline or an `&` must never reach an m3u.
pub fn watch_url_of(id: &str) -> Option<String> {
    video_id(id).map(|id| format!("https://www.youtube.com/watch?v={id}"))
}

/// Whether `id` is a well-formed video id.
pub fn is_video_id(id: &str) -> bool {
    video_id(id).is_some()
}

/// The prefix of a generic list's cache key (see [`Link::cache_key`]).
pub const WEB_KEY_PREFIX: &str = "web.";

impl Link {
    /// The video's id, when the link is YouTube's and names one.
    pub fn watch_id(&self) -> Option<&str> {
        match self {
            Link::Video { id } | Link::Both { video: id, .. } => Some(id),
            Link::Playlist { .. } | Link::Web { .. } => None,
        }
    }

    /// The playlist's id, when the link is YouTube's and names one.
    pub fn list_id(&self) -> Option<&str> {
        match self {
            Link::Playlist { id } | Link::Both { list: id, .. } => Some(id),
            Link::Video { .. } | Link::Web { .. } => None,
        }
    }

    /// The address that plays the one video the link names: YouTube's rebuilt
    /// `watch` URL, or a generic link itself.
    pub fn watch_url(&self) -> Option<String> {
        match self {
            Link::Video { id } | Link::Both { video: id, .. } => watch_url_of(id),
            Link::Playlist { .. } => None,
            Link::Web { url } => Some(url.clone()),
        }
    }

    /// The address of the list the link names: YouTube's rebuilt `playlist`
    /// URL, or a generic link itself.
    pub fn list_url(&self) -> Option<String> {
        match self {
            Link::Playlist { id } | Link::Both { list: id, .. } => {
                Some(format!("https://www.youtube.com/playlist?list={id}"))
            }
            Link::Video { .. } => None,
            Link::Web { url } => Some(url.clone()),
        }
    }

    /// The name a list's cache goes by (a file name, see
    /// `entries::ListCache`): YouTube's list id, or for a generic link `web.`
    /// and the SHA-256 of its normalised address in hex. A dot is not a
    /// character of a list id, so the two never meet.
    pub fn cache_key(&self) -> Option<String> {
        match self {
            Link::Playlist { id } | Link::Both { list: id, .. } => Some(id.clone()),
            Link::Video { .. } => None,
            Link::Web { url } => {
                use sha2::{Digest, Sha256};
                let hex: String = Sha256::digest(url.as_bytes()).iter().map(|b| format!("{b:02x}")).collect();
                Some(format!("{WEB_KEY_PREFIX}{hex}"))
            }
        }
    }
}

pub fn parse_lines(text: &str) -> Vec<(String, Result<Link, LinkError>)> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(|l| (l.to_string(), parse_link(l)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "dQw4w9WgXcQ";

    fn video(id: &str) -> Result<Link, LinkError> {
        Ok(Link::Video { id: id.into() })
    }

    fn web(url: &str) -> Result<Link, LinkError> {
        Ok(Link::Web { url: url.into() })
    }

    #[test]
    fn watch_links_become_videos() {
        for host in ["www.youtube.com", "m.youtube.com", "music.youtube.com", "youtube.com"] {
            assert_eq!(parse_link(&format!("https://{host}/watch?v={ID}")), video(ID), "{host}");
        }
    }

    #[test]
    fn short_links_are_expanded() {
        assert_eq!(parse_link(&format!("https://youtu.be/{ID}?si=abc")), video(ID));
        assert_eq!(parse_link(&format!("https://youtu.be/{ID}")), video(ID));
    }

    #[test]
    fn shorts_are_plain_videos() {
        assert_eq!(parse_link(&format!("https://www.youtube.com/shorts/{ID}")), video(ID));
    }

    #[test]
    fn tracking_parameters_are_dropped() {
        let l = parse_link(&format!("https://www.youtube.com/watch?v={ID}&t=42&si=Y&feature=share")).unwrap();
        assert_eq!(l.watch_url().unwrap(), format!("https://www.youtube.com/watch?v={ID}"));
        assert_eq!(l.list_url(), None);
    }

    #[test]
    fn playlist_links_become_playlists() {
        let l = parse_link("https://www.youtube.com/playlist?list=PLx").unwrap();
        assert_eq!(l, Link::Playlist { id: "PLx".into() });
        assert_eq!(l.list_url().unwrap(), "https://www.youtube.com/playlist?list=PLx");
        assert_eq!(l.watch_url(), None);
        assert_eq!(l.cache_key().as_deref(), Some("PLx"));
    }

    #[test]
    fn a_video_in_a_list_is_both() {
        let want = Ok(Link::Both { video: ID.into(), list: "PLx".into() });
        assert_eq!(parse_link(&format!("https://www.youtube.com/watch?v={ID}&list=PLx")), want);
        let want = Ok(Link::Both { video: ID.into(), list: "OLAK5uy_x".into() });
        assert_eq!(parse_link(&format!("https://music.youtube.com/watch?v={ID}&list=OLAK5uy_x")), want);
    }

    #[test]
    fn hosts_of_youtube_that_its_rules_refuse_stay_refused() {
        for (raw, host) in [
            (format!("https://studio.youtube.com/watch?v={ID}"), "studio.youtube.com"),
            (format!("https://foo.youtube.com/watch?v={ID}"), "foo.youtube.com"),
            (format!("https://www.youtu.be/{ID}"), "www.youtu.be"),
            (format!("https://youtube.com./watch?v={ID}"), "youtube.com."),
            (format!("https://www.youtube.com./watch?v={ID}"), "www.youtube.com."),
            (format!("https://youtu.be./{ID}"), "youtu.be."),
            (format!("https://www.youtube-nocookie.com/embed/{ID}"), "www.youtube-nocookie.com"),
            (format!("https://youtube-nocookie.com/embed/{ID}"), "youtube-nocookie.com"),
        ] {
            assert_eq!(parse_link(&raw), Err(LinkError::ForeignHost(host.into())), "{raw}");
        }
    }

    #[test]
    fn lookalikes_of_youtube_are_other_sites() {
        for raw in [
            format!("https://youtube.com.evil.example/watch?v={ID}"),
            format!("https://evil-youtube.com/watch?v={ID}"),
            format!("https://notyoutu.be/{ID}"),
            format!("https://myyoutube.com/watch?v={ID}"),
        ] {
            assert_eq!(parse_link(&raw), web(&raw), "{raw}");
        }
    }

    #[test]
    fn other_sites_are_web_links_normalised() {
        assert_eq!(
            parse_link("https://soundcloud.com/forss/flickermood"),
            web("https://soundcloud.com/forss/flickermood")
        );
        // Scheme and host lowercased, fragment dropped, query kept, a port
        // and http accepted: the network guard judges where it really goes.
        assert_eq!(
            parse_link("HTTPS://Blazo.Bandcamp.COM/album/jazz-format-mixtape-vol-1?x=1#t=30"),
            web("https://blazo.bandcamp.com/album/jazz-format-mixtape-vol-1?x=1")
        );
        assert_eq!(parse_link("http://example.org:8080/a"), web("http://example.org:8080/a"));
        // A public IP literal is a site like another.
        assert_eq!(parse_link("https://93.184.215.14/x"), web("https://93.184.215.14/x"));
        assert_eq!(parse_link("https://[2606:4700::1]/x"), web("https://[2606:4700::1]/x"));
        let l = parse_link("https://soundcloud.com/a/sets/b").unwrap();
        assert_eq!(l.watch_url().as_deref(), Some("https://soundcloud.com/a/sets/b"));
        assert_eq!(l.list_url().as_deref(), Some("https://soundcloud.com/a/sets/b"));
        assert_eq!((l.watch_id(), l.list_id()), (None, None));
    }

    #[test]
    fn a_web_list_is_cached_under_a_key_no_youtube_id_can_be() {
        let key = parse_link("https://soundcloud.com/a/sets/b").unwrap().cache_key().unwrap();
        let hex = key.strip_prefix(WEB_KEY_PREFIX).unwrap();
        assert_eq!(hex.len(), 64);
        assert!(hex.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()), "{key}");
        assert!(!key.chars().all(is_id_char), "a dot keeps it out of the list ids' grammar");
        // One key per normalised address.
        let same = parse_link("https://SOUNDCLOUD.com/a/sets/b#x").unwrap().cache_key().unwrap();
        assert_eq!(key, same);
        let other = parse_link("https://soundcloud.com/a/sets/c").unwrap().cache_key().unwrap();
        assert_ne!(key, other);
        assert_eq!(parse_link(&format!("https://youtu.be/{ID}")).unwrap().cache_key(), None);
    }

    #[test]
    fn a_link_holding_a_control_character_or_a_space_is_refused() {
        for raw in [
            "https://soundcloud.com/a\nb",
            "https://soundcloud.com/a\n#EXTINF:1,x",
            "https://soundcloud.com/a\rb",
            "https://soundcloud.com/a\tb",
            "https://soundcloud.com/a\0b",
            "https://soundcloud.com/a b",
            "https://soundcloud.com/a\u{a0}b",
            "https://soundcloud.com/a\u{85}b",
        ] {
            assert_eq!(parse_link(raw), Err(LinkError::NotAUrl), "{raw:?}");
        }
        // The same on YouTube's side: the parser would have dropped the
        // newline and accepted the id.
        assert_eq!(parse_link(&format!("https://youtu.be/{}\n{}", &ID[..5], &ID[5..])), Err(LinkError::NotAUrl));
    }

    #[test]
    fn credentials_and_other_schemes_are_refused() {
        for raw in [
            "https://user@soundcloud.com/a",
            "https://user:pw@soundcloud.com/a",
            "https://:pw@soundcloud.com/a",
            "https://youtube.com@evil.example/watch?v=dQw4w9WgXcQ",
            "file:///etc/passwd",
            "ftp://soundcloud.com/a",
            "data:text/plain,x",
            "javascript:alert(1)",
            "soundcloud.com/a",
            "https://",
        ] {
            assert_eq!(parse_link(raw), Err(LinkError::NotAUrl), "{raw}");
        }
    }

    #[test]
    fn a_local_ip_literal_is_refused_in_every_spelling() {
        for (raw, host) in [
            ("http://127.0.0.1/x", "127.0.0.1"),
            ("http://192.168.0.57:8080/", "192.168.0.57"),
            ("http://10.1.2.3/", "10.1.2.3"),
            ("http://100.64.0.1/", "100.64.0.1"),
            ("http://0x7f.1/", "127.0.0.1"),
            ("http://2130706433/", "127.0.0.1"),
            ("http://0177.0.0.1/", "127.0.0.1"),
            ("http://[::1]/", "[::1]"),
            ("http://[::ffff:192.168.1.1]/", "[::ffff:c0a8:101]"),
            ("http://[fe80::1]/", "[fe80::1]"),
            ("http://[64:ff9b::a00:1]/", "[64:ff9b::a00:1]"),
        ] {
            assert_eq!(parse_link(raw), Err(LinkError::LocalNetwork(host.into())), "{raw}");
        }
    }

    #[test]
    fn accepted_spellings_and_unsupported_schemes() {
        assert_eq!(parse_link(&format!("HTTPS://WWW.YOUTUBE.COM/watch?v={ID}")), video(ID));
        assert_eq!(parse_link(&format!("  https://youtu.be/{ID}  ")), video(ID));
        let l = parse_link(&format!("http://www.youtube.com/watch?v={ID}")).unwrap();
        assert_eq!(l.watch_url().unwrap(), format!("https://www.youtube.com/watch?v={ID}"));
        for raw in [format!("ftp://www.youtube.com/watch?v={ID}"), "javascript:alert(1)".into(), "not a url".into()] {
            assert_eq!(parse_link(&raw), Err(LinkError::NotAUrl), "{raw}");
        }
    }

    #[test]
    fn ids_are_validated() {
        for raw in [
            "https://www.youtube.com/watch?v=",
            "https://www.youtube.com/watch?v=dQw4w9WgXc!",
            "https://www.youtube.com/watch?v=dQw4w9WgXc",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQQ",
            "https://www.youtube.com/watch?v=dQw4w9WgX%2F%2F",
            "https://www.youtube.com/watch",
            "https://www.youtube.com/playlist?list=",
            "https://www.youtube.com/playlist?list=PL%20x",
            "https://www.youtube.com/playlist?list=a/b",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=bad!",
            "https://youtu.be/",
            "https://www.youtube.com/",
            "https://www.youtube.com/feed/trending",
        ] {
            assert_eq!(parse_link(raw), Err(LinkError::NoMedia), "{raw}");
        }
        assert_eq!(
            parse_link("https://www.youtube.com/playlist?list=OLAK5uy_k-Z"),
            Ok(Link::Playlist { id: "OLAK5uy_k-Z".into() })
        );
    }

    #[test]
    fn several_lines_are_parsed_independently() {
        let text = format!("https://youtu.be/{ID}\n\n   \nhttp://192.168.1.1/x\r\nhttps://evil.example/x\n");
        let out = parse_lines(&text);
        assert_eq!(out.len(), 3);
        assert_eq!(out[0], (format!("https://youtu.be/{ID}"), video(ID)));
        assert_eq!(out[1].1, Err(LinkError::LocalNetwork("192.168.1.1".into())));
        assert_eq!(out[2].1, web("https://evil.example/x"));
    }

    #[test]
    fn a_non_default_port_is_refused() {
        assert_eq!(
            parse_link(&format!("https://www.youtube.com:8443/watch?v={ID}")),
            Err(LinkError::ForeignHost("www.youtube.com:8443".into()))
        );
    }

    #[test]
    fn a_short_link_may_carry_a_list() {
        assert_eq!(
            parse_link(&format!("https://youtu.be/{ID}?list=PLx")),
            Ok(Link::Both { video: ID.into(), list: "PLx".into() })
        );
    }

    #[test]
    fn extra_path_segments_are_refused() {
        for raw in [
            format!("https://youtu.be/{ID}/extra"),
            format!("https://www.youtube.com/shorts/{ID}/extra"),
            "https://www.youtube.com/shorts/bad!".to_string(),
            "https://www.youtube.com/shorts/short".to_string(),
            format!("https://www.youtube.com/embed/{ID}/extra"),
            format!("https://www.youtube.com/v/{ID}"),
            format!("https://www.youtube.com/watch/{ID}"),
            format!("https://www.youtube.com/feed/trending?v={ID}"),
            "https://www.youtube.com/channel/x?list=PLx".to_string(),
        ] {
            assert_eq!(parse_link(&raw), Err(LinkError::NoMedia), "{raw}");
        }
    }

    #[test]
    fn repeated_parameters_are_refused() {
        for raw in [
            format!("https://www.youtube.com/watch?v={ID}&v=aaaaaaaaaaa"),
            format!("https://www.youtube.com/watch?v={ID}&list=PLx&list=PLy"),
            "https://www.youtube.com/playlist?list=PLx&list=PLy".to_string(),
            format!("https://youtu.be/{ID}?list=PLx&list=PLy"),
        ] {
            assert_eq!(parse_link(&raw), Err(LinkError::NoMedia), "{raw}");
        }
    }

    #[test]
    fn an_empty_video_id_refuses_the_whole_link_not_just_the_video() {
        assert_eq!(parse_link("https://www.youtube.com/watch?list=PLx&v="), Err(LinkError::NoMedia));
    }

    #[test]
    fn a_link_without_a_scheme_is_not_a_url() {
        assert_eq!(parse_link(&format!("youtube.com/watch?v={ID}")), Err(LinkError::NotAUrl));
    }

    #[test]
    fn embed_is_a_video_and_live_is_refused() {
        assert_eq!(parse_link(&format!("https://www.youtube.com/embed/{ID}")), video(ID));
        assert_eq!(parse_link(&format!("https://www.youtube.com/live/{ID}")), Err(LinkError::LiveStream));
    }
}
