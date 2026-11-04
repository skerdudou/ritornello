//! The system permissions the operator may decline.
//!
//! Each grant is one polkit rule the core's archive carries. A declined grant
//! is not placed: the registry records the core's rule path with the identity
//! [`DECLINED`] instead of a content identity, so the core can tell a rule
//! that is absent on purpose from one that is missing (and so never offers to
//! repair it), and the next run knows the choice without asking again.

use std::collections::BTreeSet;

use crate::registry::Registry;

/// The identity recorded for a rule path whose grant was declined.
pub const DECLINED: &str = "declined";

/// The registry key the core's record lives under (`inv.core.name`).
const CORE: &str = "core";

/// One system permission, in the order they are asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Grant {
    /// Power off or restart the system.
    Power,
    /// Change the time zone.
    Timezone,
}

impl Grant {
    pub const ALL: [Grant; 2] = [Grant::Power, Grant::Timezone];

    /// The name the command line and the documentation use.
    pub fn name(self) -> &'static str {
        match self {
            Grant::Power => "power",
            Grant::Timezone => "timezone",
        }
    }

    pub fn parse(name: &str) -> Option<Grant> {
        Grant::ALL.into_iter().find(|g| g.name() == name)
    }

    /// The polkit rule this grant places.
    pub fn rule_path(self) -> &'static str {
        match self {
            Grant::Power => "/etc/polkit-1/rules.d/50-ritornello-power.rules",
            Grant::Timezone => "/etc/polkit-1/rules.d/53-ritornello-time.rules",
        }
    }

    /// What the grant lets Ritornello do, as the summary says it.
    pub fn effect(self) -> &'static str {
        match self {
            Grant::Power => "power off / restart",
            Grant::Timezone => "change the time zone",
        }
    }

    /// The question the operator is asked.
    pub fn label(self) -> &'static str {
        match self {
            Grant::Power => "Allow Ritornello to power off or restart the system",
            Grant::Timezone => "Allow Ritornello to change the time zone",
        }
    }
}

/// The grants the device's registry records as declined: the core's record
/// carries the grant's rule path with the identity [`DECLINED`]. No registry,
/// no core record, a rule recorded with a real identity or not recorded at
/// all: not declined (a grant with no recorded choice is granted).
pub fn recorded_declined(registry: Option<&Registry>) -> BTreeSet<Grant> {
    let Some(core) = registry.and_then(|r| r.components.get(CORE)) else { return BTreeSet::new() };
    Grant::ALL
        .into_iter()
        .filter(|g| core.identity.get(g.rule_path()).is_some_and(|id| id == DECLINED))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::registry::Recorded;

    fn registry_with(core: Option<BTreeMap<String, String>>) -> Registry {
        let components = core
            .map(|identity| {
                (CORE.to_string(), Recorded { version: "0.3.0".into(), privileged: vec![], identity })
            })
            .into_iter()
            .collect();
        Registry { format: 1, components }
    }

    fn ids(pairs: &[(&str, &str)]) -> Option<BTreeMap<String, String>> {
        Some(pairs.iter().map(|(p, i)| (p.to_string(), i.to_string())).collect())
    }

    #[test]
    fn names_round_trip_and_unknown_names_are_refused() {
        for g in Grant::ALL {
            assert_eq!(Grant::parse(g.name()), Some(g));
        }
        assert_eq!(Grant::parse("Power"), None);
        assert_eq!(Grant::parse("reboot"), None);
    }

    #[test]
    fn recorded_declined_reads_only_the_core_identity() {
        let power = Grant::Power.rule_path();
        let time = Grant::Timezone.rule_path();
        assert!(recorded_declined(None).is_empty(), "no registry");
        assert!(recorded_declined(Some(&registry_with(None))).is_empty(), "no core record");
        assert_eq!(
            recorded_declined(Some(&registry_with(ids(&[(power, DECLINED)])))),
            BTreeSet::from([Grant::Power])
        );
        assert_eq!(
            recorded_declined(Some(&registry_with(ids(&[(power, DECLINED), (time, DECLINED)])))),
            BTreeSet::from([Grant::Power, Grant::Timezone])
        );
        assert!(
            recorded_declined(Some(&registry_with(ids(&[(power, "sha256:ab")])))).is_empty(),
            "a real identity is a placed rule"
        );
        assert!(recorded_declined(Some(&registry_with(ids(&[])))).is_empty(), "a registry without identities");
        assert!(
            recorded_declined(Some(&registry_with(ids(&[("/etc/other", DECLINED)])))).is_empty(),
            "another path is not a grant"
        );
    }

    /// Only the core's record speaks for a grant.
    #[test]
    fn another_components_record_is_not_the_cores_choice() {
        let mut reg = registry_with(None);
        reg.components.insert(
            "radio".into(),
            Recorded {
                version: "0.3.0".into(),
                privileged: vec![],
                identity: ids(&[(Grant::Power.rule_path(), DECLINED)]).unwrap(),
            },
        );
        assert!(recorded_declined(Some(&reg)).is_empty());
    }
}
