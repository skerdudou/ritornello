//! What a release says about the components it describes — their kinds and a
//! one-line description — so the installables dialog can describe a plugin
//! that has never run on this device.
//!
//! It also says what each component **whose archive that release carries**
//! speaks (`contracts`): the update worker reads it to judge an offered
//! archive against the core it will meet before anything is downloaded.
//!
//! Pure but for `fetch`, which is a thin wrapper over `download::fetch_text`
//! so the route and the worker read a catalogue through one door. The absence
//! of the file is a normal state and not a failure for the installables
//! dialog; for the contracts it is not, and the worker says so per row.

use crate::compat::Speaks;
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Catalogue {
    pub components: BTreeMap<String, Entry>,
    /// What each component whose archive this release carries speaks, by
    /// component name (`core` for the core). A component the release did not
    /// republish has no entry, and an entry missing is never "compatible":
    /// nothing can describe an older archive with this tree's numbers.
    ///
    /// **Not served by `GET /api/update/catalogue`** (`skip`): the page reads
    /// what the worker concluded from it on each row of `/api/update`, never
    /// the raw numbers.
    #[serde(skip)]
    pub contracts: BTreeMap<String, Speaks>,
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
        #[serde(default)]
        contracts: BTreeMap<String, serde_json::Value>,
    }
    let wire: Wire = serde_json::from_str(text)?;
    let mut components = BTreeMap::new();
    for (name, value) in wire.components {
        if let Ok(entry) = serde_json::from_value::<Entry>(value) {
            components.insert(name, entry);
        }
    }
    // Entry by entry too: a component whose contracts this core cannot read
    // (a contract it does not know, a shape from a newer release) is absent,
    // which the worker turns into "not installable from the device" — never
    // into an assumption that it is compatible.
    let mut contracts = BTreeMap::new();
    for (name, value) in wire.contracts {
        if let Ok(speaks) = serde_json::from_value::<Speaks>(value) {
            contracts.insert(name, speaks);
        }
    }
    Ok(Catalogue { components, contracts })
}

/// Fetches and reads one catalogue, bounded by the download client's own
/// limits (`download::fetch_text`: a total deadline and `TEXT_MAX`).
///
/// Every failure — a transport error, a non-200, an unreadable body —
/// collapses to `None`: the callers each have one honest answer for "could
/// not be read" (the route a 503, the worker a row it cannot vouch for).
pub async fn fetch(client: &reqwest::Client, url: &str) -> Option<Catalogue> {
    let (status, body) = crate::update::download::fetch_text(client, url).await.ok()?;
    if status != 200 {
        return None;
    }
    parse(&body).ok()
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

    /// The contracts are read when the release publishes them, entry by
    /// entry: a malformed one is skipped and the rest of the file is kept.
    #[test]
    fn the_contracts_of_the_shipped_components_are_read() {
        let text = r#"{"components":{},
            "contracts":{"core":{"protocol":2,"contracts":{"source":{"major":1,"minor":0},"admin":{"major":1,"minor":2}}},
                         "radio":{"protocol":2,"contracts":{"source":{"major":1,"minor":1}}},
                         "broken":{"protocol":"two"}}}"#;
        let c = parse(text).unwrap();
        assert_eq!(c.contracts.keys().collect::<Vec<_>>(), vec!["core", "radio"]);
        let radio = &c.contracts["radio"];
        assert_eq!(radio.protocol, 2);
        assert_eq!(
            radio.contracts.get(&ritornello_proto::Contract::Source),
            Some(&ritornello_proto::ContractVersion { major: 1, minor: 1 })
        );
    }

    /// A catalogue from before the contracts existed reads as one that
    /// publishes none — which the worker refuses to install from, rather than
    /// a parse error that would also lose the descriptions.
    #[test]
    fn a_catalogue_without_contracts_reads_with_none() {
        let c = parse(r#"{"components":{"radio":{"kinds":["source"],"description":"Stations"}}}"#).unwrap();
        assert_eq!(c.components.len(), 1);
        assert!(c.contracts.is_empty());
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
