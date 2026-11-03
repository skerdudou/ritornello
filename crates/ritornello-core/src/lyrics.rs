//! The lyrics of what is playing, as the core serves them.
//!
//! The twin of `cover`: the words themselves never ride the state frame, only
//! a local URL naming them by their content does, and the page fetches them
//! from there.

use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use ritornello_proto::{DisplayFrame, Lyrics, LyricsFrame, RenderedLyrics};
use std::collections::VecDeque;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

/// Prefix of every published lyrics URL. The rest is the key below.
pub const HREF_PREFIX: &str = "/api/lyrics/";

/// How many sets of lyrics the store keeps.
///
/// Only the retained set is ever published, so one entry would serve the
/// page; the few more cover a page or a display still fetching the previous
/// track's href while the next one is already up. At the 64 KiB ceiling
/// (`LYRICS_MAX_BYTES`) this is half a megabyte at worst, a few tens of
/// kilobytes in practice.
pub(crate) const CAPACITY: usize = 8;

/// `Cache-Control` of the route. `immutable` because the URL names its
/// content: other words get another key, hence another URL. `private`
/// because what someone listens to is nobody else's business, shared caches
/// included.
const CACHE_CONTROL: &str = "private, max-age=31536000, immutable";

/// The lyrics the core published, ready to serve, keyed like their href.
///
/// **In memory, and nothing else**: the route answers from here without any
/// I/O (no HTTP route may block, see `AGENTS.md`), and lyrics are small
/// enough that nothing needs to stay on disk. Filled by `Core::publish_state`
/// **before** the state naming them goes out, so a page never receives an
/// href this store does not hold.
///
/// A `std::sync::Mutex`, not tokio's: the core inserts from synchronous code
/// (`publish_state`), and no holder ever awaits while holding it.
pub struct LyricsStore {
    /// Most recently inserted first.
    entries: Mutex<VecDeque<(String, Arc<RenderedLyrics>)>>,
}

impl Default for LyricsStore {
    fn default() -> Self {
        Self::new()
    }
}

impl LyricsStore {
    pub fn new() -> Self {
        Self { entries: Mutex::new(VecDeque::with_capacity(CAPACITY)) }
    }

    /// Keeps `l` under `key`. A key already held only moves to the front:
    /// parsing the same LRC again would be work done for nothing.
    pub fn insert(&self, key: String, l: &Lyrics) {
        if self.touch(&key) {
            return;
        }
        // Rendered outside the lock: parsing is the only costly step, and the
        // route must not wait behind it.
        let rendered = Arc::new(RenderedLyrics::from_lyrics(l));
        let mut entries = self.entries.lock().unwrap();
        if let Some(i) = entries.iter().position(|(k, _)| *k == key) {
            entries.remove(i);
        }
        entries.push_front((key, rendered));
        entries.truncate(CAPACITY);
    }

    /// Moves `key` to the front if held. `true` if it was.
    ///
    /// What `publish_state` does with the current key at every frame: a song
    /// played again (a radio coming back to it) is then the most recent
    /// entry, never the least recent one the next insertion evicts.
    pub fn touch(&self, key: &str) -> bool {
        let mut entries = self.entries.lock().unwrap();
        let Some(i) = entries.iter().position(|(k, _)| k == key) else {
            return false;
        };
        if i > 0
            && let Some(entry) = entries.remove(i)
        {
            entries.push_front(entry);
        }
        true
    }

    /// The lyrics published under `key`, as the route serves them.
    pub fn get(&self, key: &str) -> Option<Arc<RenderedLyrics>> {
        self.entries.lock().unwrap().iter().find(|(k, _)| k == key).map(|(_, r)| r.clone())
    }

    /// The `lyrics` frame of the display protocol for `key`, encoded once
    /// and newline-terminated, for `DisplayClient::send_cover_line` (which
    /// writes any pre-encoded line as is). `href` is the state's own
    /// `lyrics_href`, carried so a display can tell which track the words
    /// belong to.
    pub fn line(&self, key: &str, href: &str) -> Option<Arc<str>> {
        let content = self.get(key)?;
        let frame = DisplayFrame::Lyrics(LyricsFrame { href: href.to_string(), content: (*content).clone() });
        match serde_json::to_string(&frame) {
            Ok(mut line) => {
                line.push('\n');
                Some(line.into())
            }
            Err(e) => {
                tracing::warn!("lyrics {key}: cannot encode the display frame: {e}");
                None
            }
        }
    }
}

/// `GET /api/lyrics/{key}`: the lyrics the state's `lyrics_href` names, as
/// `RenderedLyrics` JSON. Answered from memory only.
pub async fn lyrics_get(
    State(state): State<crate::status::AppState>,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Response {
    let Some(rendered) = state.lyrics.get(&key) else {
        // A debug, where a missing cover is a warn: a page still holding a
        // previous track's href after eight changes is ordinary, and the core
        // never publishes a key it has not stored first.
        tracing::debug!("lyrics {key} requested but not held");
        return StatusCode::NOT_FOUND.into_response();
    };
    // The key alone: it names the content, so it is the whole validator.
    let etag = format!("\"{key}\"");
    if headers.get(header::IF_NONE_MATCH).and_then(|v| v.to_str().ok()) == Some(etag.as_str()) {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    (
        [(header::CACHE_CONTROL, CACHE_CONTROL.to_string()), (header::ETAG, etag)],
        axum::Json(&*rendered),
    )
        .into_response()
}

/// Stable key of a set of lyrics, computed from their **content**.
///
/// Same hasher and same shape as `cover::content_key`, and the same trade-off:
/// a collision would show the wrong words and nothing else. Both members are
/// hashed as `Option`s, so the same text given as `plain` or as `synced` does
/// not land on one key — they are not served the same way.
///
/// Computed when the retained lyrics change (see `Metadata::lyrics_cle`),
/// never per state frame: hashing up to `LYRICS_MAX_BYTES` once a second for
/// the whole of a song would be work done for nothing.
pub fn key(l: &Lyrics) -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    l.plain.hash(&mut h);
    l.synced.hash(&mut h);
    format!("{:016x}", h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_depends_on_the_content_and_on_its_form() {
        let words = |plain: Option<&str>, synced: Option<&str>| Lyrics {
            plain: plain.map(str::to_string),
            synced: synced.map(str::to_string),
        };
        let a = words(Some("So what"), None);
        assert_eq!(key(&a), key(&a.clone()), "a key must be stable");
        assert_eq!(key(&a).len(), 16);
        assert_ne!(key(&a), key(&words(Some("So what else"), None)));
        // The same text as plain or as LRC is not served the same way.
        assert_ne!(key(&a), key(&words(None, Some("So what"))));
    }

    fn plain(text: &str) -> Lyrics {
        Lyrics { plain: Some(text.into()), synced: None }
    }

    #[test]
    fn the_store_keeps_the_most_recent_and_refreshes_a_key_it_sees_again() {
        let store = LyricsStore::new();
        for i in 0..CAPACITY {
            store.insert(format!("k{i}"), &plain(&format!("words {i}")));
        }
        // Seen again: it moves to the front instead of being evicted next.
        store.insert("k0".into(), &plain("words 0"));
        store.insert("new".into(), &plain("new words"));
        assert!(store.get("k0").is_some(), "a key published again must not be the one evicted");
        assert!(store.get("k1").is_none(), "the least recent goes once the store is full");
        assert_eq!(store.get("new").unwrap().plain.as_deref(), Some("new words"));
    }

    #[test]
    fn the_display_line_is_the_lyrics_frame_with_the_state_href() {
        let store = LyricsStore::new();
        store.insert("abc".into(), &Lyrics { plain: None, synced: Some("[00:01.00]Hi".into()) });
        let line = store.line("abc", "/api/lyrics/abc").expect("held");
        assert!(line.ends_with('\n'), "send_cover_line writes the line as is");
        let frame: DisplayFrame = serde_json::from_str(line.trim_end()).unwrap();
        let DisplayFrame::Lyrics(f) = frame else { panic!("not a lyrics frame: {line}") };
        assert_eq!(f.href, "/api/lyrics/abc");
        assert_eq!(f.content.synced.unwrap()[0].text, "Hi");
        assert!(store.line("missing", "/api/lyrics/missing").is_none());
    }
}
