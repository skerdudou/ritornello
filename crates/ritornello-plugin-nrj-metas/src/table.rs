//! Mapping from a stream URL to the brand and the station identifier.
//!
//! The table is **embedded** in the binary (`stations.toml`), collected from
//! the `/onair.json` of each of the four brand sites, where every station
//! carries its three stream URLs and its identifier in the same object.
//! `scripts/fetch-stations.mjs` regenerates it.
//!
//! **The brand is not decoration.** Measured: a host only serves its own
//! stations — asking `rireetchansons.fr` for NRJ's id 158 returns nothing. The
//! brand is therefore what tells `live` which host to query.
//!
//! It is **not** re-read from the network at startup: a device that boots
//! unattended must not depend on a third party to recognize its stations, and
//! the failure of such a fetch would be silent. An embedded table, on the
//! other hand, fails reproducibly and fixably.
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
    /// Stream tokens that designate this station, searched as whole **tokens**
    /// of the URL configured on the device (see `contains_token`).
    ///
    /// A token and not the whole URL: the published URL usually carries a
    /// query string (`?origine=fluxradios`), and the same station is served
    /// under three qualities — 128k mp3, 64k aac, HD aac — each with its own
    /// token. All three are listed, since the device may be configured with
    /// any of them.
    pub tokens: Vec<String>,
    /// Brand site that answers for this station: `nrj`, `nostalgie`,
    /// `cheriefm` or `rireetchansons`.
    pub brand: String,
    /// Identifier the metadata endpoint expects.
    pub id: u32,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Table {
    #[serde(default, rename = "station")]
    pub stations: Vec<Station>,
}

/// True if `token` appears in `url` as a **whole token**, i.e. bounded on both
/// sides by a non-alphanumeric character (or by the edge of the string).
///
/// Measured, the 765 tokens are twelve lowercase alphanumerics and none
/// contains another, so a plain substring search would work **today**. The
/// boundary rule costs nothing and removes the question for good: it is the
/// same rule `radiofrance-metas` needed, and it keeps a token from being
/// recognized inside a longer identifier NRJ might mint later.
pub fn contains_token(url: &str, token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    let bytes = url.as_bytes();
    url.match_indices(token).any(|(start, _)| {
        let end = start + token.len();
        let free_before = start == 0 || !bytes[start - 1].is_ascii_alphanumeric();
        let free_after = end >= bytes.len() || !bytes[end].is_ascii_alphanumeric();
        free_before && free_after
    })
}

impl Table {
    /// Effective table: the operator's entries first, then the embedded ones.
    ///
    /// This order gives both uses at once, with no second setting:
    /// **correcting** an entry gone stale (the same token declared in the file
    /// wins, the search stopping at the first match) and **adding** a station
    /// missing from the shipped table.
    ///
    /// Missing file: normal case, no warning. Unreadable or invalid file:
    /// warning, and we carry on with the embedded table alone rather than
    /// depriving the device of everything.
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
        self.stations.iter().find(|s| s.tokens.iter().any(|t| contains_token(url, t)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_respects_its_boundaries() {
        // The URL configured on the device carries a query string; the token
        // is what stays constant.
        assert!(contains_token("https://streaming.nrjaudio.fm/ou8o8xgk7oiu", "ou8o8xgk7oiu"));
        assert!(contains_token(
            "https://streaming.nrjaudio.fm/ou8o8xgk7oiu?origine=fluxradios",
            "ou8o8xgk7oiu"
        ));
        // A token glued to more characters is not that token.
        assert!(!contains_token("https://streaming.nrjaudio.fm/xou8o8xgk7oiuy", "ou8o8xgk7oiu"));
        // An empty token matches nothing (otherwise it would match everything).
        assert!(!contains_token("https://streaming.nrjaudio.fm/ou8o8xgk7oiu", ""));
    }

    #[test]
    fn the_embedded_table_is_valid_and_complete() {
        // `embedded()` panics on a broken table: this test is what makes the
        // plugin's logical build fail rather than its startup.
        let t = Table::embedded();
        assert!(t.stations.len() > 300, "four brands, {} stations", t.stations.len());
        let mut ids = std::collections::HashSet::new();
        let mut tokens = std::collections::HashSet::new();
        for s in &t.stations {
            assert!(!s.label.is_empty(), "station {} without label", s.id);
            assert!(!s.tokens.is_empty(), "{}: no token", s.label);
            assert!(s.id > 0, "{}: zero identifier", s.label);
            assert!(ids.insert(s.id), "{}: duplicate identifier {}", s.label, s.id);
            assert!(
                matches!(s.brand.as_str(), "nrj" | "nostalgie" | "cheriefm" | "rireetchansons"),
                "{}: unexpected brand {:?}",
                s.label,
                s.brand
            );
            for tok in &s.tokens {
                assert!(tokens.insert(tok.clone()), "{}: duplicate token {tok}", s.label);
                // Measured: all 765 tokens are exactly twelve lowercase
                // alphanumerics. A `-` or a `.` would mean a whole URL was
                // copied by mistake, and the boundary rule would no longer
                // apply as intended.
                assert_eq!(tok.len(), 12, "{}: unexpected token {tok}", s.label);
                assert!(
                    tok.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()),
                    "{}: unexpected token {tok}",
                    s.label
                );
            }
        }
    }

    #[test]
    fn no_token_swallows_another() {
        // Decisive invariant: if one station's token were recognized inside a
        // URL built on another's, the first one encountered would capture both
        // and display the wrong station's titles, with no sign at all.
        // Measured true today (no token contains another); this locks it.
        let t = Table::embedded();
        let all: Vec<&String> = t.stations.iter().flat_map(|s| &s.tokens).collect();
        for a in &all {
            for b in &all {
                if a == b {
                    continue;
                }
                assert!(!b.contains(a.as_str()), "token {a} is contained in {b}");
            }
        }
    }

    #[test]
    fn the_three_url_forms_of_a_station_are_recognized() {
        // The device may be configured with any of the three qualities.
        let t = Table::embedded();
        let rire = t.station_for("https://streaming.nrjaudio.fm/ou8o8xgk7oiu?origine=fluxradios");
        let rire = rire.expect("Rire & Chansons 128k mp3 recognized");
        assert_eq!(rire.brand, "rireetchansons");
        assert_eq!(rire.id, 200);
        for token in ["oua8afw2dqao", "out2tu6ubafg"] {
            let s = t
                .station_for(&format!("https://streaming.nrjaudio.fm/{token}"))
                .unwrap_or_else(|| panic!("{token} not recognized"));
            assert_eq!(s.id, 200, "{token} -> {}", s.label);
        }
    }

    #[test]
    fn one_station_of_each_brand_is_recognized() {
        let t = Table::embedded();
        for (token, brand, id) in [
            ("oumvmk8fnozc", "nrj", 158),
            ("oug7girb92oc", "nostalgie", 197),
            ("ouuku85n3nje", "cheriefm", 190),
            ("ou8oqegk7oiu", "rireetchansons", 38),
        ] {
            let s = t
                .station_for(&format!("https://streaming.nrjaudio.fm/{token}"))
                .unwrap_or_else(|| panic!("{token} not recognized"));
            assert_eq!((s.brand.as_str(), s.id), (brand, id), "{token} -> {}", s.label);
        }
    }

    #[test]
    fn an_unknown_url_matches_nothing() {
        let t = Table::embedded();
        assert!(t.station_for("https://ouifm3.ice.infomaniak.ch/ouifm3.mp3").is_none());
        assert!(t.station_for("https://icecast.radiofrance.fr/fip-midfi.mp3").is_none());
        assert!(t.station_for("https://www.nrj.fr/").is_none());
    }

    #[test]
    fn the_operators_file_is_consulted_before_the_embedded_table() {
        // The two uses of the file: correcting an entry gone stale, and adding
        // one missing from the shipped table.
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("nrj-metas.toml");
        std::fs::write(
            &p,
            "[[station]]\nlabel = \"correction\"\ntokens = [\"ou8o8xgk7oiu\"]\nbrand = \"nrj\"\nid = 999\n\n\
             [[station]]\nlabel = \"addition\"\ntokens = [\"newstreamtok\"]\nbrand = \"nrj\"\nid = 123\n",
        )
        .unwrap();
        let t = Table::load(&p);
        let url = "https://streaming.nrjaudio.fm/ou8o8xgk7oiu";
        assert_eq!(t.station_for(url).map(|s| s.id), Some(999), "correction");
        assert_eq!(
            t.station_for("https://x/newstreamtok").map(|s| s.id),
            Some(123),
            "addition"
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
        let p = dir.path().join("nrj.toml");
        std::fs::write(&p, "this is not toml [[[").unwrap();
        assert_eq!(Table::load(&p).stations.len(), Table::embedded().stations.len());
    }

    #[test]
    fn an_empty_token_does_not_match_everything() {
        let t: Table =
            toml::from_str("[[station]]\ntokens = [\"\"]\nbrand = \"nrj\"\nid = 1\n").unwrap();
        assert!(t.station_for("https://streaming.nrjaudio.fm/ou8o8xgk7oiu").is_none());
    }

    #[test]
    fn the_shipped_example_file_is_valid() {
        // It is meant to be copied as-is onto the device: if it failed to
        // load, the failure would be silent.
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../deploy/nrj-metas.example.toml");
        let text = std::fs::read_to_string(&p).expect("shipped example");
        toml::from_str::<Table>(&text).expect("valid example");
    }
}
