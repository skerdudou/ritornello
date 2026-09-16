use anyhow::{Context, Result};
use ritornello_proto::{Preset, Text};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Station {
    pub name: String,
    pub url: String,
    pub preset: u8,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Stations {
    #[serde(default)]
    pub stations: Vec<Station>,
}

/// Typed validation error: the user-facing text is produced at the boundary
/// via `text()`, unresolved — the core resolves it against this plugin's
/// announced catalog (language-packs chantier, task 10). `Display` provides
/// an English version for internal (dev) logs, outside the i18n perimeter.
#[derive(Debug, Clone, PartialEq)]
pub enum ValidationError {
    PresetOutOfRange { preset: u8, name: String },
    DuplicatePreset { preset: u8 },
    BadUrl { name: String, url: String },
}

impl ValidationError {
    /// Unresolved refusal surfaced to the user (body of the admin-side 422).
    ///
    /// **This site's own manifestation of a confirmed defect is gone, the
    /// underlying class is not.** The old `message(&Catalog)` built
    /// `BadUrl`'s sentence with two *chained* `.replace()` calls — `{name}`
    /// first, `{url}` second — so a station **name** that happened to
    /// contain the literal text `{url}` got rewritten by the second call,
    /// which cannot tell "a `{url}` the template put there" from "a `{url}`
    /// that arrived inside `name`" (verified: see
    /// `bad_url_names_the_station_and_the_url_even_when_the_name_contains_the_literal_placeholder`,
    /// task 9's report). This hand-written chain is gone — both parameters
    /// now travel in one map, resolved by `ritornello_core::resolve_text`.
    /// **That resolver still substitutes params one `.replace()` at a time**
    /// (over a `HashMap`, so in an unspecified order), so the same class of
    /// collision is still reachable in principle if one parameter's value
    /// contains another parameter's literal `{name}` token — it is just no
    /// longer *this crate's* hand-rolled chain doing it, and no longer
    /// deterministic. Left as a residual concern for the reviewer: fixing
    /// it belongs to `ritornello_core::resolve_text`, shared by every
    /// producer, not to one plugin's call site.
    pub fn text(&self) -> Text {
        match self {
            ValidationError::PresetOutOfRange { preset, name } => Text::Keyed {
                key: "preset_out_of_range".into(),
                params: HashMap::from([
                    ("p".to_string(), preset.to_string()),
                    ("name".to_string(), name.clone()),
                ]),
            },
            ValidationError::DuplicatePreset { preset } => Text::Keyed {
                key: "preset_duplicate".into(),
                params: HashMap::from([("p".to_string(), preset.to_string())]),
            },
            ValidationError::BadUrl { name, url } => Text::Keyed {
                key: "bad_url".into(),
                params: HashMap::from([
                    ("name".to_string(), name.clone()),
                    ("url".to_string(), url.clone()),
                ]),
            },
        }
    }
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::PresetOutOfRange { preset, name } => {
                write!(f, "preset {preset} out of range 1-99 ({name})")
            }
            ValidationError::DuplicatePreset { preset } => write!(f, "duplicate preset {preset}"),
            ValidationError::BadUrl { name, url } => write!(f, "invalid URL for {name}: {url}"),
        }
    }
}

impl std::error::Error for ValidationError {}

impl Stations {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading {}", path.display()))?;
        let s: Stations = toml::from_str(&text)?;
        s.validate()?;
        Ok(s)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        self.validate()?;
        // Like `state::save` and `Bindings::save`: on a pristine machine
        // without /etc/ritornello, the first "Save" of the admin page failed
        // with a raw I/O error.
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, toml::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn validate(&self) -> std::result::Result<(), ValidationError> {
        let mut seen = std::collections::HashSet::new();
        for s in &self.stations {
            if !(1..=99).contains(&s.preset) {
                return Err(ValidationError::PresetOutOfRange { preset: s.preset, name: s.name.clone() });
            }
            if !seen.insert(s.preset) {
                return Err(ValidationError::DuplicatePreset { preset: s.preset });
            }
            if !s.url.starts_with("http://") && !s.url.starts_with("https://") {
                return Err(ValidationError::BadUrl { name: s.name.clone(), url: s.url.clone() });
            }
        }
        Ok(())
    }

    pub fn by_preset(&self, preset: u8) -> Option<&Station> {
        self.stations.iter().find(|s| s.preset == preset)
    }

    /// The station whose URL is `url`, if it is still in the table.
    ///
    /// Used to find **where what is playing went** after the table was
    /// reshuffled: the preset is a *position*, so reordering the stations
    /// makes the memorized number point at another station. The URL is what
    /// durably identifies a stream — the name depends on the device's
    /// configuration, and the number on its order.
    pub fn by_url(&self, url: &str) -> Option<&Station> {
        self.stations.iter().find(|s| s.url == url)
    }

    pub fn next_preset(&self, from: u8) -> Option<u8> {
        let mut p: Vec<u8> = self.stations.iter().map(|s| s.preset).collect();
        p.sort_unstable();
        p.iter().copied().find(|x| *x > from).or_else(|| p.first().copied())
    }

    pub fn prev_preset(&self, from: u8) -> Option<u8> {
        let mut p: Vec<u8> = self.stations.iter().map(|s| s.preset).collect();
        p.sort_unstable();
        p.iter().rev().copied().find(|x| *x < from).or_else(|| p.last().copied())
    }

    /// Highest preset number in the table — what the web grid shows. Through
    /// the admin presets are contiguous 1..N so this is also the count; a
    /// hand-edited sparse table just exposes a few empty numbers, answered
    /// by the existing "empty preset" transient.
    pub fn preset_count(&self) -> u8 {
        self.stations.iter().map(|s| s.preset).max().unwrap_or(0)
    }

    /// The named presets of the table, **sorted by number**.
    ///
    /// The list may be sparse (stations 1, 5, 99: `preset_count` is its
    /// maximum, not its length, and the two legitimately diverge). The order of
    /// the MPD positions follows this list: a hand-edited `stations.toml` whose
    /// file order does not match the numbers must therefore not produce an
    /// out-of-order list on the client.
    pub fn presets(&self) -> Vec<Preset> {
        let mut v: Vec<Preset> =
            self.stations.iter().map(|s| Preset { index: s.preset, name: s.name.clone() }).collect();
        v.sort_by_key(|p| p.index);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Stations {
        Stations {
            stations: vec![
                Station { name: "FIP".into(), url: "http://icecast.radiofrance.fr/fip-midfi.mp3".into(), preset: 1 },
                Station { name: "France Inter".into(), url: "http://icecast.radiofrance.fr/franceinter-midfi.mp3".into(), preset: 3 },
            ],
        }
    }

    #[test]
    fn roundtrip_toml() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stations.toml");
        sample().save(&path).unwrap();
        assert_eq!(Stations::load(&path).unwrap(), sample());
    }

    #[test]
    fn by_preset_finds_the_station() {
        assert_eq!(sample().by_preset(3).unwrap().name, "France Inter");
        assert!(sample().by_preset(2).is_none());
    }

    #[test]
    fn next_prev_preset_wrap_around() {
        let s = sample();
        assert_eq!(s.next_preset(1), Some(3));
        assert_eq!(s.next_preset(3), Some(1)); // wraps around
        assert_eq!(s.prev_preset(3), Some(1));
        assert_eq!(s.prev_preset(1), Some(3)); // wraps around
    }

    #[test]
    fn validate_rejects_duplicates_and_out_of_range() {
        let mut s = sample();
        s.stations[1].preset = 1;
        assert!(s.validate().is_err());
        let mut s2 = sample();
        s2.stations[0].preset = 100;
        assert!(s2.validate().is_err());
        let mut s3 = sample();
        s3.stations[0].url = "ftp://nope".into();
        assert!(s3.validate().is_err());
    }

    #[test]
    fn validation_produces_a_typed_error() {
        let mut s = sample();
        s.stations[0].preset = 100;
        assert!(matches!(
            s.validate(),
            Err(ValidationError::PresetOutOfRange { preset: 100, .. })
        ));
        let mut d = sample();
        d.stations[1].preset = 1;
        assert!(matches!(d.validate(), Err(ValidationError::DuplicatePreset { preset: 1 })));
    }

    #[test]
    fn validation_accepts_presets_up_to_99() {
        // A two-digit preset beyond 9 is now valid.
        let mut s = sample();
        s.stations[0].preset = 42;
        assert!(s.validate().is_ok());

        // Still rejected beyond 99.
        let mut too_high = sample();
        too_high.stations[0].preset = 100;
        assert!(matches!(
            too_high.validate(),
            Err(ValidationError::PresetOutOfRange { preset: 100, .. })
        ));

        // And 0 is still rejected (the lower bound does not move).
        let mut zero = sample();
        zero.stations[0].preset = 0;
        assert!(matches!(
            zero.validate(),
            Err(ValidationError::PresetOutOfRange { preset: 0, .. })
        ));
    }

    #[test]
    fn the_count_is_the_highest_preset() {
        // Table with holes (hand-edited): the count follows the highest
        // number, not the number of stations.
        let s = Stations {
            stations: vec![
                Station { name: "A".into(), url: "http://a".into(), preset: 1 },
                Station { name: "B".into(), url: "http://b".into(), preset: 5 },
                Station { name: "C".into(), url: "http://c".into(), preset: 9 },
            ],
        };
        assert_eq!(s.preset_count(), 9);
        assert_eq!(Stations::default().preset_count(), 0);
    }

    #[test]
    fn stations_enumerate_with_their_names_and_numbers() {
        let s: Stations = toml::from_str(
            r#"
                [[stations]]
                name = "FIP"
                url = "https://exemple/fip.mp3"
                preset = 1
                [[stations]]
                name = "Nova"
                url = "https://exemple/nova.mp3"
                preset = 5
            "#,
        )
        .unwrap();
        assert_eq!(
            s.presets(),
            vec![
                Preset { index: 1, name: "FIP".into() },
                Preset { index: 5, name: "Nova".into() },
            ]
        );
    }

    #[test]
    fn enumeration_is_sorted_by_number_not_by_file_order() {
        // The MPD positions will follow this order: a hand-edited
        // stations.toml must not give an out-of-order list on the client. The
        // file below declares Nova (5) before FIP (1): if `presets()` settled
        // for the table order, the rendered list would start with Nova.
        let s: Stations = toml::from_str(
            r#"
                [[stations]]
                name = "Nova"
                url = "https://exemple/nova.mp3"
                preset = 5
                [[stations]]
                name = "FIP"
                url = "https://exemple/fip.mp3"
                preset = 1
                [[stations]]
                name = "France Inter"
                url = "https://exemple/inter.mp3"
                preset = 3
            "#,
        )
        .unwrap();
        assert_eq!(
            s.presets(),
            vec![
                Preset { index: 1, name: "FIP".into() },
                Preset { index: 3, name: "France Inter".into() },
                Preset { index: 5, name: "Nova".into() },
            ]
        );
    }

    #[test]
    fn every_refusal_names_a_key_that_exists_in_the_embedded_catalog() {
        // The plugin no longer resolves (no `Catalog` left — language-packs
        // chantier, task 10): what this test still owns is that the key is
        // not a typo.
        let known = ritornello_i18n::try_parse(crate::RADIO_EN).unwrap();
        for t in [
            ValidationError::PresetOutOfRange { preset: 10, name: "X".into() }.text(),
            ValidationError::DuplicatePreset { preset: 3 }.text(),
            ValidationError::BadUrl { name: "X".into(), url: "ftp://x".into() }.text(),
        ] {
            match t {
                Text::Keyed { key, .. } => assert!(known.contains_key(&key), "unknown key: {key}"),
                Text::Verbatim(s) => panic!("a validation refusal must be a key, not verbatim: {s}"),
            }
        }
    }

    #[test]
    fn preset_out_of_range_carries_both_parameters() {
        let err = ValidationError::PresetOutOfRange { preset: 10, name: "X".into() };
        match err.text() {
            Text::Keyed { key, params } => {
                assert_eq!(key, "preset_out_of_range");
                assert_eq!(params.get("p").map(String::as_str), Some("10"));
                assert_eq!(params.get("name").map(String::as_str), Some("X"));
            }
            Text::Verbatim(_) => panic!("expected a keyed text"),
        }
    }

    /// The regression a review found and the brief asked to verify at this
    /// task: a *chained* `.replace("{name}", name).replace("{url}", url)`
    /// rewrote a `{url}` that arrived **inside** `name`, indistinguishable
    /// from the template's own placeholder. Both parameters now travel
    /// independently in the same map, so a station named literally
    /// `"my station {url}"` keeps that text unrewritten in its own slot —
    /// there is no second `.replace()` pass at this call site to corrupt it.
    ///
    /// **[MUTATION]**: go back to `catalog.get("bad_url").replace("{name}",
    /// name).replace("{url}", url)` (reintroducing the chain) — this test
    /// would then require running through `ritornello_core::resolve_text`
    /// to observe the corruption, which this unit alone cannot reproduce;
    /// what this test actually proves is the **narrower**, mechanically
    /// checkable claim: the `name` parameter is carried verbatim, unmodified
    /// by anything in this function.
    #[test]
    fn bad_url_carries_the_name_unmodified_even_when_it_contains_the_url_placeholder_text() {
        let err =
            ValidationError::BadUrl { name: "my station {url}".into(), url: "ftp://x".into() };
        match err.text() {
            Text::Keyed { params, .. } => {
                assert_eq!(params.get("name").map(String::as_str), Some("my station {url}"));
                assert_eq!(params.get("url").map(String::as_str), Some("ftp://x"));
            }
            Text::Verbatim(_) => panic!("expected a keyed text"),
        }
    }
}
