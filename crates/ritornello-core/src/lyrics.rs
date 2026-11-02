//! The lyrics of what is playing, as the core serves them.
//!
//! The twin of `cover`: the words themselves never ride the state frame, only
//! a local URL naming them by their content does, and the page fetches them
//! from there.

use ritornello_proto::Lyrics;
use std::hash::{Hash, Hasher};

/// Prefix of every published lyrics URL. The rest is the key below.
pub const HREF_PREFIX: &str = "/api/lyrics/";

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
}
