//! One version per wire contract.
//!
//! A *contract* is the set of messages that **travel** on one kind of socket,
//! not the file a type happens to be declared in: `PlayerState` is declared in
//! `metadata.rs` but only travels to displays, so it belongs to the display
//! contract; `Repeat` travels on input, source and display, so a change to it
//! moves all three.
//!
//! The rule for each version: the **major** moves on a break of that
//! contract's API, the **minor** on a compatible addition, and the minor
//! resets to 0 when the major moves.
//!
//! **Adding a contract** is not a version of any contract: it moves
//! `PROTOCOL_VERSION` (in `lib.rs`). A contract name is a closed enum on the
//! wire, so an older core cannot read an announcement that names a new one,
//! and a new contract comes with a new kind or socket: every component must
//! be republished together, which the bootstrap move makes the release script
//! demand. (The older core reports such a plugin as silent, since it cannot
//! parse the announcement at all; reading unknown names leniently was
//! considered and refused.)

use crate::register::PluginKind;
use serde::{Deserialize, Serialize};
use std::fmt;

/// A wire contract. Serialised under its lowercase name, which is also the
/// name a plugin's announcement uses as a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Contract {
    Source,
    Display,
    Input,
    Metadata,
    Admin,
}

/// A `major.minor` contract version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractVersion {
    pub major: u32,
    pub minor: u32,
}

impl ContractVersion {
    pub const fn new(major: u32, minor: u32) -> Self {
        Self { major, minor }
    }
}

impl fmt::Display for ContractVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

// `scripts/changed-components.sh` reads these five lines with sed: keep each
// one on a single line, in exactly this shape.
pub const SOURCE_CONTRACT: ContractVersion = ContractVersion::new(1, 1);
pub const DISPLAY_CONTRACT: ContractVersion = ContractVersion::new(1, 0);
pub const INPUT_CONTRACT: ContractVersion = ContractVersion::new(1, 0);
pub const METADATA_CONTRACT: ContractVersion = ContractVersion::new(1, 0);
pub const ADMIN_CONTRACT: ContractVersion = ContractVersion::new(1, 0);

impl Contract {
    /// Every contract, in wire order.
    pub const ALL: [Contract; 5] = [
        Contract::Source,
        Contract::Display,
        Contract::Input,
        Contract::Metadata,
        Contract::Admin,
    ];

    /// The version of this contract that this build speaks.
    pub fn current(self) -> ContractVersion {
        match self {
            Contract::Source => SOURCE_CONTRACT,
            Contract::Display => DISPLAY_CONTRACT,
            Contract::Input => INPUT_CONTRACT,
            Contract::Metadata => METADATA_CONTRACT,
            Contract::Admin => ADMIN_CONTRACT,
        }
    }

    /// The contract a plugin kind speaks.
    pub fn of_kind(kind: PluginKind) -> Contract {
        match kind {
            PluginKind::Source => Contract::Source,
            PluginKind::Display => Contract::Display,
            PluginKind::Input => Contract::Input,
            PluginKind::Metadata => Contract::Metadata,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contracts_travel_under_their_lowercase_names() {
        let names: Vec<String> = Contract::ALL.iter().map(|c| serde_json::to_string(c).unwrap()).collect();
        assert_eq!(names, ["\"source\"", "\"display\"", "\"input\"", "\"metadata\"", "\"admin\""]);
    }

    #[test]
    fn every_kind_has_its_contract() {
        assert_eq!(Contract::of_kind(PluginKind::Source), Contract::Source);
        assert_eq!(Contract::of_kind(PluginKind::Display), Contract::Display);
        assert_eq!(Contract::of_kind(PluginKind::Input), Contract::Input);
        assert_eq!(Contract::of_kind(PluginKind::Metadata), Contract::Metadata);
    }

    #[test]
    fn a_version_reads_as_major_dot_minor() {
        assert_eq!(ContractVersion::new(1, 2).to_string(), "1.2");
        assert_eq!(
            serde_json::to_string(&ContractVersion::new(1, 2)).unwrap(),
            r#"{"major":1,"minor":2}"#
        );
    }

    #[test]
    fn every_contract_starts_at_one_zero_and_source_has_moved_once() {
        for c in Contract::ALL {
            // The source contract went to 1.1 for `play_request`, the only
            // compatible addition so far; every other one is still at 1.0.
            let expected = match c {
                Contract::Source => ContractVersion::new(1, 1),
                _ => ContractVersion::new(1, 0),
            };
            assert_eq!(c.current(), expected, "{c:?}");
        }
    }

    #[test]
    fn the_bootstrap_number_is_two() {
        assert_eq!(crate::PROTOCOL_VERSION, 2);
    }
}
