//! Mapping from a stream URL to the station identifier.
//!
//! The table is **embedded** in the binary (`stations.toml`), collected from
//! the public documentation of the Radio France Open API, where each station
//! carries both its `liveStream` (hence its Icecast mount) and its
//! `playerUrl` (which contains `id_station=<n>`). `scripts/fetch-stations.mjs`
//! regenerates it.
//!
//! It is **not** re-read from the network at startup: a device that boots
//! unattended must not depend on a third-party page to recognize its
//! stations, and the failure of such a fetch would be silent. An embedded
//! table, on the other hand, fails reproducibly and fixably.
//!
//! A configuration file is still consulted **first**: it makes it possible to
//! correct an entry gone stale or to add one, without recompiling.

use serde::Deserialize;
use std::path::Path;

/// Table shipped with the binary.
const EMBEDDED: &str = include_str!("stations.toml");

#[derive(Debug, Clone, Deserialize)]
pub struct Station {
    /// Label, for the logs and the readability of the file. Never displayed.
    #[serde(default)]
    pub label: String,
    /// Mounts that designate this station, searched as whole **tokens** of
    /// the URL configured in `stations.toml` (see `contains_token`).
    ///
    /// A mount and not the whole URL: Radio France serves the same station
    /// under at least three forms — `icecast.radiofrance.fr/<mount>-midfi.mp3`,
    /// the historic name `direct.fipradio.fr/live/<mount>-midfi.mp3` (which
    /// redirects to the first, hence the one directories reference), and the
    /// HLS `stream.radiofrance.fr/<mount>/<mount>.m3u8` — not counting the
    /// qualities (`-lofi`, `-hifi.aac`). The mount is the only common part.
    pub mounts: Vec<String>,
    /// Identifier expected by the live-feed endpoint.
    pub id: u32,
    /// Rendering profile to request for this station (last segment of the
    /// live URL, see `live::live_url`).
    ///
    /// Only two values, and the choice is not cosmetic: it is what decides
    /// whether the plugin says anything. `webrf_fip_player` on Mouv' returns
    /// the station's slogan and nothing else.
    ///
    /// The default value serves the entries written by hand in the operator's
    /// file: it is the profile of the music stations, the most likely for a
    /// station one would bother adding.
    #[serde(default = "default_profile")]
    pub rules: String,
}

/// Profile used when an entry declares none.
fn default_profile() -> String {
    "webrf_fip_player".to_string()
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Table {
    #[serde(default, rename = "station")]
    pub stations: Vec<Station>,
}

/// True if `mount` appears in `url` as a **whole token**, i.e. bounded on
/// both sides by a non-alphanumeric character (or by the edge of the string).
///
/// A plain substring search does not fit here: `fip` is a prefix of
/// `fipgroove`, `francemusique` of `francemusiquebaroque`, and the first
/// entry encountered would capture all the others, displaying the titles of
/// the wrong station, with no sign at all. The boundary rule settles the case
/// once and for all and leaves one entry per station, instead of forcing the
/// choice of fragments long enough not to swallow each other.
///
/// It handles the three URL forms well: `/fip-midfi.mp3` (bounded by `/` and
/// `-`), `/fip/fip.m3u8` (by `/` and `/`, then `/` and `.`),
/// `fip_midfi.m3u8` (by `/` and `_`) — and rejects `fipradio.fr` as well as
/// `fipgroove-midfi.mp3`.
pub fn contains_token(url: &str, mount: &str) -> bool {
    if mount.is_empty() {
        return false;
    }
    let bytes = url.as_bytes();
    url.match_indices(mount).any(|(start, _)| {
        let end = start + mount.len();
        let free_before = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        let free_after = end >= bytes.len() || !bytes[end].is_ascii_alphanumeric();
        free_before && free_after
    })
}

impl Table {
    /// Effective table: the operator's entries first, then the embedded ones.
    ///
    /// This order gives both uses at once, with no second setting:
    /// **correcting** an entry gone stale (the same mount declared in the
    /// file wins, the search stopping at the first match) and **adding** a
    /// station missing from the shipped table.
    ///
    /// Missing file: normal case, no warning — the embedded table is enough.
    /// Unreadable or invalid file: warning, and we carry on with the embedded
    /// table alone rather than depriving the device of everything.
    pub fn load(path: &Path) -> Self {
        let mut stations = Vec::new();
        match std::fs::read_to_string(path) {
            Ok(text) => match toml::from_str::<Self>(&text) {
                Ok(t) => {
                    tracing::info!("{} station(s) declared in {}", t.stations.len(), path.display());
                    stations.extend(t.stations);
                }
                Err(e) => tracing::warn!("{} is invalid ({e}): bundled table only", path.display()),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("{} is unreadable ({e}): bundled table only", path.display()),
        }
        stations.extend(Self::embedded().stations);
        Self { stations }
    }

    /// Embedded table alone. An unreadable shipped table would be a build
    /// defect of the plugin, not an operational error: hence the `expect`,
    /// locked in by a test.
    pub fn embedded() -> Self {
        toml::from_str(EMBEDDED).expect("valid embedded station table")
    }

    /// Station matching this stream URL, if there is one. First match, in
    /// table order.
    pub fn station_for(&self, url: &str) -> Option<&Station> {
        self.stations.iter().find(|s| s.mounts.iter().any(|m| contains_token(url, m)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_respects_its_boundaries() {
        assert!(contains_token("https://icecast.radiofrance.fr/fip-midfi.mp3", "fip"));
        assert!(contains_token("https://stream.radiofrance.fr/fip/fip.m3u8", "fip"));
        assert!(contains_token("https://stream.radiofrance.fr/fip/fip_midfi.m3u8", "fip"));
        assert!(contains_token("https://direct.fipradio.fr/live/fip-midfi.mp3", "fip"));
        // The heart of the problem: a prefix must not capture the others.
        assert!(!contains_token("https://icecast.radiofrance.fr/fipgroove-midfi.mp3", "fip"));
        assert!(!contains_token("https://direct.fipradio.fr/live/fipgroove-midfi.mp3", "fip"));
        assert!(!contains_token("https://icecast.radiofrance.fr/francemusiquebaroque-midfi.mp3", "francemusique"));
        // An empty mount matches nothing (otherwise it would match everything).
        assert!(!contains_token("https://icecast.radiofrance.fr/fip-midfi.mp3", ""));
    }

    #[test]
    fn a_non_ascii_character_counts_as_a_boundary() {
        // A URL can carry anything; the rule must not panic on a UTF-8
        // continuation byte at the edge of the token.
        assert!(contains_token("https://example.test/é/fip/é", "fip"));
    }

    #[test]
    fn no_mount_swallows_another() {
        // Decisive invariant: if the mount of one entry were recognized as a
        // token in a URL built on the mount of another, the first one
        // encountered would capture both stations and display the titles of
        // the wrong one, with no sign at all.
        let t = Table::embedded();
        for a in &t.stations {
            for b in &t.stations {
                if a.id == b.id {
                    continue;
                }
                for ma in &a.mounts {
                    for mb in &b.mounts {
                        let url = format!("https://icecast.radiofrance.fr/{mb}-midfi.mp3");
                        assert!(
                            !contains_token(&url, ma),
                            "\"{ma}\" ({}) captures the URL of \"{mb}\" ({})",
                            a.label,
                            b.label
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn every_station_carries_a_known_profile() {
        // An unknown profile does not fail the request: the server answers,
        // but with nothing more than the station's slogan. The failure would
        // therefore be silent, hence this lock.
        let t = Table::embedded();
        for s in &t.stations {
            assert!(
                matches!(s.rules.as_str(), "webrf_fip_player" | "webrf_mouv_player"),
                "{}: unexpected profile {:?}",
                s.label,
                s.rules
            );
        }
        // The music stations whose response splits title and artist.
        for (id, expected) in [
            (7, "webrf_fip_player"),
            (66, "webrf_fip_player"),
            (411, "webrf_fip_player"),
            // Mouv' and the local stations: measured, only this profile
            // yields the track.
            (6, "webrf_mouv_player"),
            (12, "webrf_mouv_player"),
            (1, "webrf_mouv_player"),
            (4, "webrf_mouv_player"),
        ] {
            let s = t.stations.iter().find(|s| s.id == id).unwrap();
            assert_eq!(s.rules, expected, "station {id} ({})", s.label);
        }
    }

    #[test]
    fn an_entry_without_profile_takes_the_default_one() {
        // The operator's file must remain writable by hand, without knowing
        // this field.
        let t: Table = toml::from_str("[[station]]\nmounts = [\"x\"]\nid = 1\n").unwrap();
        assert_eq!(t.stations[0].rules, "webrf_fip_player");
    }

    #[test]
    fn the_embedded_table_is_valid_and_complete() {
        // `embedded()` panics on a broken table: this test is what makes the
        // plugin's logical build fail rather than its startup.
        let t = Table::embedded();
        assert_eq!(t.stations.len(), 74, "6 brands + 12 FIP webradios + 11 France Musique + 45 locals");
        let mut ids = std::collections::HashSet::new();
        let mut mounts = std::collections::HashSet::new();
        for s in &t.stations {
            assert!(!s.label.is_empty(), "station {} without label", s.id);
            assert!(!s.mounts.is_empty(), "{}: no mount", s.label);
            assert!(s.mounts.iter().all(|m| !m.is_empty()), "{}: empty mount", s.label);
            assert!(s.id > 0, "{}: zero identifier", s.label);
            assert!(ids.insert(s.id), "{}: duplicate identifier {}", s.label, s.id);
            for m in &s.mounts {
                assert!(mounts.insert(m.clone()), "{}: duplicate mount {m}", s.label);
                // The collected mounts are alphanumeric; a `-` or a `.` would
                // signal that a whole URL was copied by mistake, and the
                // boundary rule would no longer apply as intended.
                assert!(
                    m.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
                    "{}: unexpected mount {m}",
                    s.label
                );
            }
        }
    }

    #[test]
    fn recognizes_the_three_url_forms_of_the_same_station() {
        // The URL registered by the operator can come from a directory, the
        // website or a player: only the mount is common to all three.
        let t = Table::embedded();
        for url in [
            "https://icecast.radiofrance.fr/fipgroove-midfi.mp3",
            "https://icecast.radiofrance.fr/fipgroove-hifi.aac",
            "https://direct.fipradio.fr/live/fipgroove-midfi.mp3",
            "https://stream.radiofrance.fr/fipgroove/fipgroove.m3u8",
        ] {
            assert_eq!(t.station_for(url).map(|s| s.id), Some(66), "{url}");
        }
    }

    #[test]
    fn the_major_stations_and_the_locals_are_recognized() {
        let t = Table::embedded();
        for (url, expected) in [
            ("https://icecast.radiofrance.fr/franceinter-midfi.mp3", 1),
            ("https://icecast.radiofrance.fr/franceinfo-midfi.mp3", 2),
            ("https://icecast.radiofrance.fr/francemusique-midfi.mp3", 4),
            ("https://icecast.radiofrance.fr/franceculture-lofi.mp3", 5),
            ("https://icecast.radiofrance.fr/mouv-midfi.mp3", 6),
            ("https://icecast.radiofrance.fr/fip-midfi.mp3", 7),
            // Local station whose mount does not look like its on-air name.
            ("https://icecast.radiofrance.fr/fbfrequenzamora-midfi.mp3", 11),
            ("https://icecast.radiofrance.fr/fb1071-midfi.mp3", 68),
            // France Musique webradio whose mount is just as misleading.
            ("https://icecast.radiofrance.fr/francemusiquelabo-midfi.mp3", 407),
        ] {
            let s = t.station_for(url).unwrap_or_else(|| panic!("{url} not recognized"));
            assert_eq!(s.id, expected, "{url} -> {}", s.label);
        }
    }

    #[test]
    fn an_unknown_url_matches_nothing() {
        // The most common case: any other station configured on the device.
        let t = Table::embedded();
        assert!(t.station_for("https://ouifm3.ice.infomaniak.ch/ouifm3.mp3").is_none());
        assert!(t.station_for("https://somafm.com/groovesalad256.pls").is_none());
        // The Radio France website is not a stream, but it carries the same
        // words: that URL has no business being in `stations.toml`, and if it
        // were, recognizing the station would still be the lesser evil.
        assert!(t.station_for("https://www.radiofrance.fr/").is_none());
    }

    #[test]
    fn the_operators_file_is_consulted_before_the_embedded_table() {
        // The two uses of the file: correcting an entry gone stale, and
        // adding one missing from the shipped table.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("radiofrance-metas.toml");
        std::fs::write(
            &p,
            "[[station]]\nlabel = \"correction\"\nmounts = [\"fip\"]\nid = 999\n\n\
             [[station]]\nlabel = \"addition\"\nmounts = [\"newstream\"]\nid = 123\n",
        )
        .unwrap();
        let t = Table::load(&p);
        let url = "https://icecast.radiofrance.fr/fip-midfi.mp3";
        assert_eq!(t.station_for(url).map(|s| s.id), Some(999), "correction");
        assert_eq!(t.station_for("https://x/newstream.mp3").map(|s| s.id), Some(123), "addition");
        // The rest of the embedded table keeps answering.
        assert_eq!(
            t.station_for("https://icecast.radiofrance.fr/fipreggae-midfi.mp3").map(|s| s.id),
            Some(71),
            "Reggae still known"
        );
    }

    #[test]
    fn a_missing_file_leaves_the_embedded_table_intact() {
        let dir = tempfile::tempdir().unwrap();
        let t = Table::load(&dir.path().join("absent.toml"));
        assert_eq!(t.stations.len(), Table::embedded().stations.len());
    }

    #[test]
    fn an_invalid_file_leaves_the_embedded_table_intact() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("rf.toml");
        std::fs::write(&p, "this is not toml [[[").unwrap();
        assert_eq!(Table::load(&p).stations.len(), Table::embedded().stations.len());
    }

    #[test]
    fn an_empty_mount_does_not_match_everything() {
        // Without the guard in `contains_token`, a badly filled entry would
        // make this station queried for **all** URLs.
        let t: Table = toml::from_str("[[station]]\nmounts = [\"\"]\nid = 1\n").unwrap();
        assert!(t.station_for("https://icecast.radiofrance.fr/fip-midfi.mp3").is_none());
        let empty: Table = toml::from_str("[[station]]\nmounts = []\nid = 1\n").unwrap();
        assert!(empty.station_for("https://x/y").is_none());
    }

    #[test]
    fn the_shipped_example_file_is_valid() {
        // It is meant to be copied as-is onto the device: if it failed to
        // load, the failure would be silent.
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../deploy/radiofrance-metas.example.toml");
        let text = std::fs::read_to_string(&p).expect("shipped example");
        toml::from_str::<Table>(&text).expect("valid example");
    }
}
