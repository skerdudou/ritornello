//! What a release says about the components it describes — their kinds and a
//! one-line description — so the installables dialog can describe a plugin
//! that has never run on this device.
//!
//! Pure, like `release`: the network lives in `download`. The absence of the
//! file is a normal state and not a failure, because every release published
//! before this chantier has none.

use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Catalogue {
    pub components: BTreeMap<String, Entry>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    pub kinds: Vec<String>,
    pub description: String,
}

/// Reads a catalogue, skipping the entries it cannot read.
///
/// Entry by entry rather than all or nothing, and the idiom is
/// `parse_checksums`': a catalogue that gained a field in a newer release
/// must not make the whole file unreadable on an older core. A skipped entry
/// degrades to a row with a name and no description, which is exactly what a
/// release with no catalogue at all already shows.
pub fn parse(text: &str) -> Result<Catalogue, serde_json::Error> {
    #[derive(serde::Deserialize)]
    struct Wire {
        #[serde(default)]
        components: BTreeMap<String, serde_json::Value>,
    }
    let wire: Wire = serde_json::from_str(text)?;
    let mut components = BTreeMap::new();
    for (name, value) in wire.components {
        if let Ok(entry) = serde_json::from_value::<Entry>(value) {
            components.insert(name, entry);
        }
    }
    Ok(Catalogue { components })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A malformed entry is skipped, the rest of the file is kept.
    ///
    /// Same idiom as `parse_checksums`: a catalogue that gained a field must
    /// not make the whole thing unreadable, and a missing entry degrades to a
    /// name with no description — which is exactly what a release without any
    /// catalogue already does.
    #[test]
    fn a_malformed_entry_is_skipped_and_the_others_are_kept() {
        let text = r#"{"components":{"radio":{"kinds":["source"],"description":"Stations"},
                        "broken":{"kinds":"source"}}}"#;
        let c = parse(text).unwrap();
        assert_eq!(c.components.len(), 1);
        assert_eq!(c.components["radio"].kinds, vec!["source"]);
    }

    /// A body that is not a catalogue at all is an error, not an empty
    /// catalogue: "GitHub answered a rate-limit page" and "this release
    /// publishes nothing" are different facts, and the second already has a
    /// representation.
    #[test]
    fn a_body_that_is_not_json_is_an_error() {
        assert!(parse("<html>rate limited</html>").is_err());
    }
}
