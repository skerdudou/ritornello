//! The core's verdict on a plugin, contract by contract.
//!
//! A plugin announces one version per wire contract it speaks (see
//! `ritornello_proto::contract`). This module is the **only** judge of those
//! announcements, called at both doors (startup and hot re-announcement), so
//! the two can never disagree.
//!
//! What a verdict means for the plugin:
//! - `Refused`: nothing is wired and the process is stopped, as before.
//! - `Accepted` with a non-empty `limited` list: the plugin is wired and
//!   working, but it speaks a newer minor of those contracts than the core
//!   does, so some of its features are inactive. Its other contracts are
//!   normal.
//!
//! One refused contract refuses the whole plugin: a half-wired plugin is a
//! state the core already avoids, since a plugin is "connected" only if all
//! of its kinds are.
//!
//! Order of checks, the first that applies wins: legacy binary, missing
//! contract, unexpected contract, major gap, then minor gaps.

use ritornello_proto::{Announcement, Contract, ContractVersion, PROTOCOL_VERSION};
use serde::{Deserialize, Serialize};

/// A contract on which the plugin and the core do not speak the same version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractGap {
    pub contract: Contract,
    pub plugin: ContractVersion,
    pub core: ContractVersion,
}

/// Why a plugin is refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Refusal {
    /// Built before contract versions existed (or against another bootstrap).
    Legacy { found: u32 },
    /// A kind (or the admin page) announced without its contract version.
    MissingContract { contract: Contract },
    /// A contract version announced for a kind or admin page it does not declare.
    UnexpectedContract { contract: Contract },
    /// One or more contracts whose major differs from the core's.
    Major { gaps: Vec<ContractGap> },
}

/// The core's decision on one announcement: wired (possibly with some features
/// inactive on the contracts listed in `limited`) or refused for the given reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Accepted { limited: Vec<ContractGap> },
    Refused(Refusal),
}

/// Judge an announcement against the contracts this build speaks.
pub fn judge(a: &Announcement) -> Verdict {
    judge_against(a, |c| c.current())
}

fn judge_against(a: &Announcement, core: impl Fn(Contract) -> ContractVersion) -> Verdict {
    if a.protocol != PROTOCOL_VERSION {
        return Verdict::Refused(Refusal::Legacy { found: a.protocol });
    }

    let expected = |c: Contract| (a.admin && c == Contract::Admin) || a.kinds.iter().any(|k| Contract::of_kind(*k) == c);

    if let Some(contract) = Contract::ALL.into_iter().find(|c| expected(*c) && !a.contracts.contains_key(c)) {
        return Verdict::Refused(Refusal::MissingContract { contract });
    }
    if let Some(contract) = Contract::ALL.into_iter().find(|c| a.contracts.contains_key(c) && !expected(*c)) {
        return Verdict::Refused(Refusal::UnexpectedContract { contract });
    }

    let gap = |c: Contract| ContractGap { contract: c, plugin: a.contracts[&c], core: core(c) };

    let major: Vec<ContractGap> = Contract::ALL
        .into_iter()
        .filter(|c| a.contracts.get(c).is_some_and(|v| v.major != core(*c).major))
        .map(gap)
        .collect();
    if !major.is_empty() {
        return Verdict::Refused(Refusal::Major { gaps: major });
    }

    let limited = Contract::ALL
        .into_iter()
        .filter(|c| a.contracts.get(c).is_some_and(|v| v.minor > core(*c).minor))
        .map(gap)
        .collect();
    Verdict::Accepted { limited }
}

/// One English sentence naming why a plugin was refused, for the log.
///
/// The status page does not read this: it receives the structured `Refusal`
/// and writes its own sentence in the operator's language.
pub fn describe(refusal: &Refusal) -> String {
    let gap = |g: &ContractGap| format!("{} {} (this core speaks {})", name(g.contract), g.plugin, g.core);
    match refusal {
        Refusal::Legacy { found } => format!(
            "it speaks protocol {found} and this core speaks {PROTOCOL_VERSION}: a binary built before contract versions, or for another core"
        ),
        Refusal::MissingContract { contract } => {
            format!("it declares the {} contract but announces no version for it", name(*contract))
        }
        Refusal::UnexpectedContract { contract } => {
            format!("it announces a version of the {} contract, which it does not declare", name(*contract))
        }
        Refusal::Major { gaps } => {
            format!("another major on {}", gaps.iter().map(gap).collect::<Vec<_>>().join(", "))
        }
    }
}

/// One English sentence listing the contracts on which an accepted plugin is
/// limited, for the log.
pub fn describe_limits(limited: &[ContractGap]) -> String {
    limited
        .iter()
        .map(|g| format!("{} {} is newer than this core's {}, some features are inactive", name(g.contract), g.plugin, g.core))
        .collect::<Vec<_>>()
        .join("; ")
}

/// A contract's wire name, the one the announcement and the page use.
fn name(contract: Contract) -> &'static str {
    match contract {
        Contract::Source => "source",
        Contract::Display => "display",
        Contract::Input => "input",
        Contract::Metadata => "metadata",
        Contract::Admin => "admin",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ritornello_proto::PluginKind;

    fn ann(kinds: &[PluginKind], admin: bool, contracts: &[(Contract, (u32, u32))]) -> Announcement {
        Announcement {
            name: "test".into(),
            kinds: kinds.to_vec(),
            admin,
            covers: false,
            ui_version: None,
            protocol: PROTOCOL_VERSION,
            version: None,
            repository: None,
            catalog: None,
            contracts: contracts.iter().map(|(c, (ma, mi))| (*c, ContractVersion::new(*ma, *mi))).collect(),
        }
    }

    fn gap(contract: Contract, plugin: (u32, u32), core: (u32, u32)) -> ContractGap {
        ContractGap { contract, plugin: ContractVersion::new(plugin.0, plugin.1), core: ContractVersion::new(core.0, core.1) }
    }

    #[test]
    fn a_plugin_speaking_the_core_s_versions_is_accepted_without_limits() {
        let a = ann(&[PluginKind::Source], true, &[(Contract::Source, (1, 0)), (Contract::Admin, (1, 0))]);
        assert_eq!(judge(&a), Verdict::Accepted { limited: vec![] });
    }

    #[test]
    fn a_pre_contract_binary_is_refused_as_legacy_not_as_missing_a_contract() {
        let mut a = ann(&[PluginKind::Source], false, &[]);
        a.protocol = 1;
        assert_eq!(judge(&a), Verdict::Refused(Refusal::Legacy { found: 1 }));
    }

    #[test]
    fn a_kind_without_its_contract_is_refused_and_named() {
        let a = ann(&[PluginKind::Display, PluginKind::Input], false, &[(Contract::Display, (1, 0))]);
        assert_eq!(judge(&a), Verdict::Refused(Refusal::MissingContract { contract: Contract::Input }));
    }

    #[test]
    fn an_admin_page_without_its_contract_is_refused() {
        let a = ann(&[PluginKind::Source], true, &[(Contract::Source, (1, 0))]);
        assert_eq!(judge(&a), Verdict::Refused(Refusal::MissingContract { contract: Contract::Admin }));
    }

    #[test]
    fn a_contract_the_plugin_does_not_declare_is_refused() {
        let a = ann(&[PluginKind::Source], false, &[(Contract::Source, (1, 0)), (Contract::Admin, (1, 0))]);
        assert_eq!(judge(&a), Verdict::Refused(Refusal::UnexpectedContract { contract: Contract::Admin }));
    }

    #[test]
    fn another_major_refuses_the_whole_plugin_and_lists_every_gap() {
        let a = ann(&[PluginKind::Display, PluginKind::Input], false, &[(Contract::Display, (2, 0)), (Contract::Input, (0, 3))]);
        assert_eq!(
            judge(&a),
            Verdict::Refused(Refusal::Major {
                gaps: vec![gap(Contract::Display, (2, 0), (1, 0)), gap(Contract::Input, (0, 3), (1, 0))]
            })
        );
    }

    #[test]
    fn a_newer_minor_is_accepted_but_limited_on_that_contract_only() {
        let a = ann(
            &[PluginKind::Display, PluginKind::Input],
            true,
            &[(Contract::Display, (1, 1)), (Contract::Input, (1, 0)), (Contract::Admin, (1, 0))],
        );
        assert_eq!(judge(&a), Verdict::Accepted { limited: vec![gap(Contract::Display, (1, 1), (1, 0))] });
    }

    #[test]
    fn an_older_minor_is_normal() {
        // Every current minor is 0, so the rule is exercised against a core at 1.3.
        let a = ann(&[PluginKind::Display], false, &[(Contract::Display, (1, 1))]);
        assert_eq!(judge_against(&a, |_| ContractVersion::new(1, 3)), Verdict::Accepted { limited: vec![] });
    }

    #[test]
    fn a_missing_contract_outranks_a_major_gap() {
        // Display is at another major, and input is absent: the absence is named.
        let a = ann(&[PluginKind::Display, PluginKind::Input], false, &[(Contract::Display, (2, 0))]);
        assert_eq!(judge(&a), Verdict::Refused(Refusal::MissingContract { contract: Contract::Input }));
    }

    #[test]
    fn an_unexpected_contract_outranks_a_major_gap() {
        let a = ann(&[PluginKind::Source], false, &[(Contract::Source, (2, 0)), (Contract::Admin, (1, 0))]);
        assert_eq!(judge(&a), Verdict::Refused(Refusal::UnexpectedContract { contract: Contract::Admin }));
    }

    #[test]
    fn duplicate_kinds_expect_their_contract_once() {
        let a = ann(&[PluginKind::Source, PluginKind::Source], false, &[(Contract::Source, (1, 0))]);
        assert_eq!(judge(&a), Verdict::Accepted { limited: vec![] });
    }

    #[test]
    fn a_plugin_declaring_nothing_and_announcing_nothing_is_accepted() {
        let a = ann(&[], false, &[]);
        assert_eq!(judge(&a), Verdict::Accepted { limited: vec![] });
    }

    // The web UI reads these shapes: changing one is a wire change.
    #[test]
    fn a_refusal_serialises_with_its_reason_tag() {
        let major = Refusal::Major { gaps: vec![gap(Contract::Display, (2, 0), (1, 0))] };
        assert_eq!(
            serde_json::to_value(&major).unwrap(),
            serde_json::json!({"reason":"major","gaps":[{"contract":"display","plugin":{"major":2,"minor":0},"core":{"major":1,"minor":0}}]})
        );
        assert_eq!(serde_json::to_value(Refusal::Legacy { found: 1 }).unwrap(), serde_json::json!({"reason":"legacy","found":1}));
        assert_eq!(
            serde_json::to_value(Refusal::MissingContract { contract: Contract::Input }).unwrap(),
            serde_json::json!({"reason":"missing_contract","contract":"input"})
        );
        assert_eq!(
            serde_json::to_value(Refusal::UnexpectedContract { contract: Contract::Admin }).unwrap(),
            serde_json::json!({"reason":"unexpected_contract","contract":"admin"})
        );
    }

    #[test]
    fn a_gap_serialises_with_both_versions() {
        assert_eq!(
            serde_json::to_value(gap(Contract::Input, (1, 2), (1, 0))).unwrap(),
            serde_json::json!({"contract":"input","plugin":{"major":1,"minor":2},"core":{"major":1,"minor":0}})
        );
    }

    #[test]
    fn a_major_gap_outranks_a_minor_gap() {
        let a = ann(&[PluginKind::Display, PluginKind::Input], false, &[(Contract::Display, (2, 0)), (Contract::Input, (1, 1))]);
        assert_eq!(judge(&a), Verdict::Refused(Refusal::Major { gaps: vec![gap(Contract::Display, (2, 0), (1, 0))] }));
    }

    #[test]
    fn a_refusal_is_described_in_one_sentence_naming_its_cause() {
        assert_eq!(
            describe(&Refusal::Legacy { found: 1 }),
            format!("it speaks protocol 1 and this core speaks {PROTOCOL_VERSION}: a binary built before contract versions, or for another core")
        );
        assert_eq!(
            describe(&Refusal::MissingContract { contract: Contract::Input }),
            "it declares the input contract but announces no version for it"
        );
        assert_eq!(
            describe(&Refusal::UnexpectedContract { contract: Contract::Admin }),
            "it announces a version of the admin contract, which it does not declare"
        );
        assert_eq!(
            describe(&Refusal::Major {
                gaps: vec![gap(Contract::Display, (2, 0), (1, 0)), gap(Contract::Input, (0, 3), (1, 0))]
            }),
            "another major on display 2.0 (this core speaks 1.0), input 0.3 (this core speaks 1.0)"
        );
    }

    #[test]
    fn limits_are_described_contract_by_contract() {
        assert_eq!(
            describe_limits(&[gap(Contract::Display, (1, 1), (1, 0)), gap(Contract::Admin, (1, 2), (1, 0))]),
            "display 1.1 is newer than this core's 1.0, some features are inactive; admin 1.2 is newer than this core's 1.0, some features are inactive"
        );
    }

    #[test]
    fn a_contract_is_described_under_its_wire_name() {
        for c in Contract::ALL {
            assert_eq!(serde_json::to_value(c).unwrap(), serde_json::json!(name(c)));
        }
    }
}
