//! Mapping from a stream URL to the metadata identifier.
//!
//! The table is **embedded** in the binary (`webradios.toml`), collected from
//! OUI FM's source of truth: the JavaScript variable `apidata` of the player
//! page, where each stream carries its stream identifier (`id`) and its
//! metadata identifier (`idMds`). `scripts/fetch-webradios.mjs` regenerates
//! it.
//!
//! It is **not** re-read from the website at startup: the list only lives in
//! an HTML page, and a regex extraction over a page a third party redesigns
//! whenever it wants is too fragile for a device that must boot unattended —
//! its failure would be silent, and the device would lose the titles without
//! a word. An embedded table, on the other hand, fails reproducibly and
//! fixably.
//!
//! A configuration file is still consulted **first**: it makes it possible to
//! correct an entry gone stale or to add one, without recompiling.

use serde::Deserialize;
use std::path::Path;

/// Table shipped with the binary.
const EMBEDDED: &str = include_str!("webradios.toml");

#[derive(Debug, Clone, Deserialize)]
pub struct Webradio {
    /// Label, for the logs and the readability of the file. Never displayed.
    #[serde(default)]
    pub label: String,
    /// URL fragments that designate this webradio, searched as
    /// **substrings** of the URL configured in `stations.toml`.
    ///
    /// Substrings and not the whole URL: the broadcast URL carries a signed
    /// token and a format parameter that vary (`?format=hd`, `sd`, `hls`),
    /// but it always contains the stream identifier.
    ///
    /// Several fragments because the same webradio is broadcast under **two
    /// URL forms**: the `streams.lesindesradios.fr` one (the one the website
    /// uses today) and the historic Icecast mount
    /// (`ouifm3.ice.infomaniak.ch/ouifm3.mp3`). The latter is the one met in
    /// practice — long published, hence referenced by directories and copied
    /// by users. Knowing only one of them amounted to recognizing no station
    /// added the normal way.
    pub urls: Vec<String>,
    /// Identifier expected by the metadata stream's `?id=` (`idMds` at OUI
    /// FM). **Distinct from `stream`**: checked by hand, the stream
    /// identifier yields an empty frame, with neither artist nor title.
    pub metas: String,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Table {
    #[serde(default, rename = "webradio")]
    pub webradios: Vec<Webradio>,
}

impl Table {
    /// Effective table: the operator's entries first, then the embedded ones.
    ///
    /// This order gives both uses at once, with no second setting:
    /// **correcting** an entry gone stale (the same `stream` declared in the
    /// file wins, the search stopping at the first match) and **adding** a
    /// stream missing from the shipped table.
    ///
    /// Missing file: normal case, no warning — the embedded table is enough.
    /// Unreadable or invalid file: warning, and we carry on with the embedded
    /// table alone rather than depriving the device of everything.
    pub fn load(path: &Path) -> Self {
        let mut webradios = Vec::new();
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<Self>(&text) {
                Ok(t) => {
                    tracing::info!("{} webradio(s) declared in {}", t.webradios.len(), path.display());
                    webradios.extend(t.webradios);
                }
                Err(e) => tracing::warn!("{} invalid ({e}): embedded table only", path.display()),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("{} unreadable ({e}): embedded table only", path.display()),
        }
        webradios.extend(Self::embedded().webradios);
        Self { webradios }
    }

    /// Embedded table alone. An unreadable shipped table would be a build
    /// defect of the plugin, not an operational error: hence the `expect`,
    /// locked in by a test.
    pub fn embedded() -> Self {
        toml::from_str(EMBEDDED).expect("valid embedded webradio table")
    }

    /// Webradio matching this stream URL, if there is one. First match, in
    /// table order.
    pub fn metas_for(&self, url: &str) -> Option<&Webradio> {
        self.webradios
            .iter()
            .find(|w| w.urls.iter().any(|f| !f.is_empty() && url.contains(f.as_str())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real stream URL, as OUI FM serves it (signed token included).
    const CLASSIC_ROCK_URL: &str = "https://streams.lesindesradios.fr/play/radios/oui-fm/3qhtSltZ27/any/300/11d46a.NND%2BFTMcarOrumMD%2FJU7lENzKQUNWno%2FSz7wPrtsPIw%3D?format=hd";

    #[test]
    fn no_fragment_swallows_another() {
        // Decisive invariant: if a fragment of one entry were contained in a
        // fragment of another, the first one encountered would capture both
        // stations and display the titles of the wrong one, with no sign at
        // all. A fragment that is too short (`ouifm` instead of `ouifm3.`)
        // would be enough to cause exactly that.
        let t = Table::embedded();
        for a in &t.webradios {
            for b in &t.webradios {
                if a.metas == b.metas {
                    continue;
                }
                for fa in &a.urls {
                    for fb in &b.urls {
                        assert!(
                            !fb.contains(fa.as_str()),
                            "\"{fa}\" ({}) is contained in \"{fb}\" ({})",
                            a.label,
                            b.label
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_historic_mounts_are_in_the_table() {
        // These are the URLs published long ago, hence the ones a directory
        // references and a user copies: without them, an OUI FM station added
        // the normal way was recognized by no entry.
        let t = Table::embedded();
        for (url, expected) in [
            ("https://ouifm.ice.infomaniak.ch/ouifm-high.mp3", "2174546520932614531"),
            ("https://ouifm3.ice.infomaniak.ch/ouifm3.mp3", "3134161803443976427"),
            ("https://ouifm2.ice.infomaniak.ch/ouifm2.mp3", "3134161803443976382"),
            ("https://ouifm5.ice.infomaniak.ch/ouifm5.mp3", "3134161803443976526"),
        ] {
            let w = t.metas_for(url).unwrap_or_else(|| panic!("{url} not recognized"));
            assert_eq!(w.metas, expected, "{url} -> {}", w.label);
        }
    }

    #[test]
    fn the_embedded_table_is_valid_and_complete() {
        // `embedded()` panics on a broken table: this test is what makes the
        // plugin's logical build fail rather than its startup.
        let t = Table::embedded();
        assert!(t.webradios.len() >= 20, "21 streams collected, {} found", t.webradios.len());
        for w in &t.webradios {
            assert!(!w.urls.is_empty(), "{}: no URL fragment", w.label);
            assert!(w.urls.iter().all(|u| !u.is_empty()), "{}: empty fragment", w.label);
            assert!(!w.metas.is_empty(), "{}: empty metadata identifier", w.label);
            // The two identifiers are of different natures at OUI FM (a short
            // alphanumeric token, a large decimal number): confusing them
            // would yield an empty frame, with neither artist nor title, and
            // no error sign at all. Checked by hand on the real stream.
            assert!(
                !w.urls.contains(&w.metas),
                "{}: metadata identifier used as a URL fragment",
                w.label
            );
            assert!(
                w.metas.chars().all(|c| c.is_ascii_digit()),
                "{}: `metas` must be a numeric mds identifier, found {:?}",
                w.label,
                w.metas
            );
        }
    }

    #[test]
    fn recognizes_a_real_stream_url() {
        let t = Table::embedded();
        let w = t.metas_for(CLASSIC_ROCK_URL).expect("Classic Rock recognized");
        assert_eq!(w.metas, "3134161803443976427");
        assert_eq!(w.label, "Oüi FM Classic Rock");
    }

    #[test]
    fn recognizes_the_same_station_whatever_the_format_or_token() {
        // The URL registered by the operator can differ from the one
        // collected: the token is signed and the format is a choice. Only the
        // stream identifier is stable, and recognition rests on it.
        let t = Table::embedded();
        for url in [
            "https://streams.lesindesradios.fr/play/radios/oui-fm/3qhtSltZ27/any/300/autre-jeton?format=sd",
            "https://streams.lesindesradios.fr/play/radios/oui-fm/3qhtSltZ27/any/300/x?format=hls",
            "http://exemple.test/3qhtSltZ27",
        ] {
            assert_eq!(t.metas_for(url).map(|w| w.metas.as_str()), Some("3134161803443976427"), "{url}");
        }
    }

    #[test]
    fn an_unknown_url_matches_nothing() {
        // The most common case: any other station configured on the device.
        let t = Table::embedded();
        assert!(t.metas_for("http://icecast.radiofrance.fr/fip-midfi.mp3").is_none());
    }

    #[test]
    fn the_operators_file_is_consulted_before_the_embedded_table() {
        // The two uses of the file: correcting an entry gone stale, and
        // adding one missing from the shipped table.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ouifm-metas.toml");
        std::fs::write(
            &p,
            "[[webradio]]\nlabel = \"correction\"\nurls = [\"3qhtSltZ27\"]\nmetas = \"999\"\n\n\
             [[webradio]]\nlabel = \"ajout\"\nurls = [\"nouveau-stream\"]\nmetas = \"123\"\n",
        )
        .unwrap();
        let t = Table::load(&p);
        assert_eq!(t.metas_for(CLASSIC_ROCK_URL).map(|w| w.metas.as_str()), Some("999"), "correction");
        assert_eq!(t.metas_for("http://x/nouveau-stream").map(|w| w.metas.as_str()), Some("123"), "addition");
        // The rest of the embedded table keeps answering.
        assert!(t.metas_for("http://x/fkYz8mdU3T").is_some(), "Rock Inde still known");
    }

    #[test]
    fn a_missing_file_leaves_the_embedded_table_intact() {
        let dir = tempfile::tempdir().unwrap();
        let t = Table::load(&dir.path().join("absent.toml"));
        assert_eq!(t.webradios.len(), Table::embedded().webradios.len());
    }

    #[test]
    fn an_invalid_file_leaves_the_embedded_table_intact() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("ouifm.toml");
        std::fs::write(&p, "ceci n'est pas du toml [[[").unwrap();
        assert_eq!(Table::load(&p).webradios.len(), Table::embedded().webradios.len());
    }

    #[test]
    fn an_empty_fragment_does_not_match_everything() {
        // Without this guard, `"".contains` being always true, a badly filled
        // entry would make this stream queried for **all** stations.
        let t: Table = toml::from_str("[[webradio]]\nurls = [\"\"]\nmetas = \"1\"\n").unwrap();
        assert!(t.metas_for("http://icecast.radiofrance.fr/fip-midfi.mp3").is_none());
        // And an entry with no fragment at all matches nothing either.
        let empty: Table = toml::from_str("[[webradio]]\nurls = []\nmetas = \"1\"\n").unwrap();
        assert!(empty.metas_for("http://x/y").is_none());
    }

    #[test]
    fn the_shipped_example_file_is_valid() {
        // It is meant to be copied as-is onto the device: if it failed to
        // load, the failure would be silent.
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../deploy/ouifm-metas.example.toml");
        let text = std::fs::read_to_string(&p).expect("shipped example");
        toml::from_str::<Table>(&text).expect("valid example");
    }
}
