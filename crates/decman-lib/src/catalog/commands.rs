//! Governance flow builders.
//!
//! Thirteen pure builders cover confirm, execute, expire, and cancel for
//! all three governance kinds (vault, core self-management, core domain
//! actions), plus the atomic two-command proposal retraction. Every
//! builder builds one choice-argument record, wraps it in an
//! `ExerciseCommand`, and hands the command(s) to
//! [`commands_envelope`](crate::framework::commands::commands_envelope) —
//! `member` fills both `act_as` and the choice's own actor field
//! (`confirmer` / `executor` / `member`); `governance_party` fills
//! `read_as`. Template ids arrive already resolved (see
//! `catalog::templates`); package-ref resolution and the actual ledger
//! submission stay with the caller.

use canton_proto_rs::com::daml::ledger::api::v2::{
    Command, Commands, DisclosedContract, ExerciseCommand, Optional, Value, command, value,
};
use common::canton_id::CantonId;

use crate::catalog::action::ActionType;
use crate::error::Error;
use crate::framework::TemplateId;
use crate::framework::commands::commands_envelope;
use crate::framework::encode::{field, make_contract_id, make_list, make_party, make_record};

/// One `ExerciseCommand`, wrapped as a `Command`.
fn exercise(template: &TemplateId, contract_id: &str, choice: &str, argument: Value) -> Command {
    Command {
        command: Some(command::Command::Exercise(ExerciseCommand {
            template_id: Some(template.into()),
            contract_id: contract_id.to_string(),
            choice: choice.to_string(),
            choice_argument: Some(argument),
        })),
    }
}

/// The empty-record argument every no-op-payload choice (the three
/// `_Cancel` choices and `GovernableAction_ProposerCancel`) takes.
fn empty_record() -> Value {
    make_record(vec![])
}

/// A `[ContractId]` list value from confirmation cid strings.
fn confirmations_list(confirmation_cids: &[String]) -> Value {
    make_list(
        confirmation_cids
            .iter()
            .map(|cid| make_contract_id(cid))
            .collect(),
    )
}

// ============================================================================
// Confirm
// ============================================================================

/// `VaultGovernanceRules_ConfirmAction`: `{confirmer, action}`.
pub fn build_confirm_vault_action(
    rules: &TemplateId,
    rules_cid: &str,
    member: &CantonId,
    governance_party: &CantonId,
    action: &ActionType,
    command_id: String,
) -> Result<Commands, Error> {
    let argument = make_record(vec![
        field("confirmer", make_party(member)),
        field("action", action.to_vault_proto()?),
    ]);
    let cmd = exercise(
        rules,
        rules_cid,
        "VaultGovernanceRules_ConfirmAction",
        argument,
    );
    Ok(commands_envelope(
        command_id,
        member,
        governance_party,
        vec![cmd],
        vec![],
    ))
}

/// `GovernanceRules_ConfirmGovernanceAction`: `{confirmer, action}`.
pub fn build_confirm_self_action(
    rules: &TemplateId,
    rules_cid: &str,
    member: &CantonId,
    governance_party: &CantonId,
    action: &ActionType,
    command_id: String,
) -> Result<Commands, Error> {
    let argument = make_record(vec![
        field("confirmer", make_party(member)),
        field("action", action.to_self_proto()?),
    ]);
    let cmd = exercise(
        rules,
        rules_cid,
        "GovernanceRules_ConfirmGovernanceAction",
        argument,
    );
    Ok(commands_envelope(
        command_id,
        member,
        governance_party,
        vec![cmd],
        vec![],
    ))
}

/// `GovernanceRules_ConfirmAction`: `{confirmer, actionProposalCid}`.
pub fn build_confirm_proposal(
    rules: &TemplateId,
    rules_cid: &str,
    member: &CantonId,
    governance_party: &CantonId,
    proposal_cid: &str,
    command_id: String,
) -> Commands {
    let argument = make_record(vec![
        field("confirmer", make_party(member)),
        field("actionProposalCid", make_contract_id(proposal_cid)),
    ]);
    let cmd = exercise(rules, rules_cid, "GovernanceRules_ConfirmAction", argument);
    commands_envelope(command_id, member, governance_party, vec![cmd], vec![])
}

// ============================================================================
// Execute
// ============================================================================

/// `VaultGovernanceRules_ExecuteConfirmedAction`:
/// `{executor, action, confirmations, contractCid}`.
#[allow(clippy::too_many_arguments)]
pub fn build_execute_vault_action(
    rules: &TemplateId,
    rules_cid: &str,
    member: &CantonId,
    governance_party: &CantonId,
    action: &ActionType,
    confirmation_cids: &[String],
    contract_cid: Option<&str>,
    disclosed: Vec<DisclosedContract>,
    command_id: String,
) -> Result<Commands, Error> {
    let contract_cid_value = Value {
        sum: Some(value::Sum::Optional(Box::new(Optional {
            value: contract_cid.map(|cid| Box::new(make_contract_id(cid))),
        }))),
    };
    let argument = make_record(vec![
        field("executor", make_party(member)),
        field("action", action.to_vault_proto()?),
        field("confirmations", confirmations_list(confirmation_cids)),
        field("contractCid", contract_cid_value),
    ]);
    let cmd = exercise(
        rules,
        rules_cid,
        "VaultGovernanceRules_ExecuteConfirmedAction",
        argument,
    );
    Ok(commands_envelope(
        command_id,
        member,
        governance_party,
        vec![cmd],
        disclosed,
    ))
}

/// `GovernanceRules_ExecuteGovernanceAction`: `{executor, action, confirmations}`.
#[allow(clippy::too_many_arguments)]
pub fn build_execute_self_action(
    rules: &TemplateId,
    rules_cid: &str,
    member: &CantonId,
    governance_party: &CantonId,
    action: &ActionType,
    confirmation_cids: &[String],
    disclosed: Vec<DisclosedContract>,
    command_id: String,
) -> Result<Commands, Error> {
    let argument = make_record(vec![
        field("executor", make_party(member)),
        field("action", action.to_self_proto()?),
        field("confirmations", confirmations_list(confirmation_cids)),
    ]);
    let cmd = exercise(
        rules,
        rules_cid,
        "GovernanceRules_ExecuteGovernanceAction",
        argument,
    );
    Ok(commands_envelope(
        command_id,
        member,
        governance_party,
        vec![cmd],
        disclosed,
    ))
}

/// `GovernanceRules_ExecuteConfirmedAction`: `{executor, actionProposalCid, confirmations}`.
#[allow(clippy::too_many_arguments)]
pub fn build_execute_proposal(
    rules: &TemplateId,
    rules_cid: &str,
    member: &CantonId,
    governance_party: &CantonId,
    proposal_cid: &str,
    confirmation_cids: &[String],
    disclosed: Vec<DisclosedContract>,
    command_id: String,
) -> Commands {
    let argument = make_record(vec![
        field("executor", make_party(member)),
        field("actionProposalCid", make_contract_id(proposal_cid)),
        field("confirmations", confirmations_list(confirmation_cids)),
    ]);
    let cmd = exercise(
        rules,
        rules_cid,
        "GovernanceRules_ExecuteConfirmedAction",
        argument,
    );
    commands_envelope(command_id, member, governance_party, vec![cmd], disclosed)
}

// ============================================================================
// Expire
// ============================================================================

/// The `{member, staleConfirmationCid}` argument all three expire choices share.
fn expire_argument(member: &CantonId, stale_confirmation_cid: &str) -> Value {
    make_record(vec![
        field("member", make_party(member)),
        field(
            "staleConfirmationCid",
            make_contract_id(stale_confirmation_cid),
        ),
    ])
}

/// `VaultGovernanceRules_ExpireConfirmation`: `{member, staleConfirmationCid}`.
pub fn build_expire_vault_confirmation(
    rules: &TemplateId,
    rules_cid: &str,
    member: &CantonId,
    governance_party: &CantonId,
    confirmation_cid: &str,
    command_id: String,
) -> Commands {
    let argument = expire_argument(member, confirmation_cid);
    let cmd = exercise(
        rules,
        rules_cid,
        "VaultGovernanceRules_ExpireConfirmation",
        argument,
    );
    commands_envelope(command_id, member, governance_party, vec![cmd], vec![])
}

/// `GovernanceRules_ExpireGovernanceSelfConfirmation`: `{member, staleConfirmationCid}`.
pub fn build_expire_self_confirmation(
    rules: &TemplateId,
    rules_cid: &str,
    member: &CantonId,
    governance_party: &CantonId,
    confirmation_cid: &str,
    command_id: String,
) -> Commands {
    let argument = expire_argument(member, confirmation_cid);
    let cmd = exercise(
        rules,
        rules_cid,
        "GovernanceRules_ExpireGovernanceSelfConfirmation",
        argument,
    );
    commands_envelope(command_id, member, governance_party, vec![cmd], vec![])
}

/// `GovernanceRules_ExpireConfirmation`: `{member, staleConfirmationCid}`.
pub fn build_expire_domain_confirmation(
    rules: &TemplateId,
    rules_cid: &str,
    member: &CantonId,
    governance_party: &CantonId,
    confirmation_cid: &str,
    command_id: String,
) -> Commands {
    let argument = expire_argument(member, confirmation_cid);
    let cmd = exercise(
        rules,
        rules_cid,
        "GovernanceRules_ExpireConfirmation",
        argument,
    );
    commands_envelope(command_id, member, governance_party, vec![cmd], vec![])
}

// ============================================================================
// Cancel
// ============================================================================

/// `VaultGovernanceConfirmation_Cancel`: empty record.
pub fn build_cancel_vault_confirmation(
    confirmation: &TemplateId,
    confirmation_cid: &str,
    member: &CantonId,
    governance_party: &CantonId,
    command_id: String,
) -> Commands {
    let cmd = exercise(
        confirmation,
        confirmation_cid,
        "VaultGovernanceConfirmation_Cancel",
        empty_record(),
    );
    commands_envelope(command_id, member, governance_party, vec![cmd], vec![])
}

/// `GovernanceSelfConfirmation_Cancel`: empty record.
pub fn build_cancel_self_confirmation(
    confirmation: &TemplateId,
    confirmation_cid: &str,
    member: &CantonId,
    governance_party: &CantonId,
    command_id: String,
) -> Commands {
    let cmd = exercise(
        confirmation,
        confirmation_cid,
        "GovernanceSelfConfirmation_Cancel",
        empty_record(),
    );
    commands_envelope(command_id, member, governance_party, vec![cmd], vec![])
}

/// `GovernanceConfirmation_Cancel`: empty record.
pub fn build_cancel_domain_confirmation(
    confirmation: &TemplateId,
    confirmation_cid: &str,
    member: &CantonId,
    governance_party: &CantonId,
    command_id: String,
) -> Commands {
    let cmd = exercise(
        confirmation,
        confirmation_cid,
        "GovernanceConfirmation_Cancel",
        empty_record(),
    );
    commands_envelope(command_id, member, governance_party, vec![cmd], vec![])
}

/// Retract a proposal: `GovernableAction_ProposerCancel` on the proposal
/// itself, plus (when the caller has one) `GovernanceConfirmation_Cancel`
/// on their own confirmation of it — both empty-record, in one atomic
/// submission so they succeed or fail together.
///
/// `GovernableAction_ProposerCancel` is declared on the `GovernableAction`
/// interface rather than any one template, so the exercise carries the
/// interface id, never the id of the template that actually created the
/// proposal contract.
pub fn build_cancel_proposal(
    interface: &TemplateId,
    proposal_cid: &str,
    own_confirmation: Option<(&str, &TemplateId)>,
    member: &CantonId,
    governance_party: &CantonId,
    command_id: String,
) -> Commands {
    let mut cmds = vec![exercise(
        interface,
        proposal_cid,
        "GovernableAction_ProposerCancel",
        empty_record(),
    )];
    if let Some((confirmation_cid, confirmation_template)) = own_confirmation {
        cmds.push(exercise(
            confirmation_template,
            confirmation_cid,
            "GovernanceConfirmation_Cancel",
            empty_record(),
        ));
    }
    commands_envelope(command_id, member, governance_party, cmds, vec![])
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::daml::ledger::api::v2::command;

    use super::*;
    use crate::framework::record::extract_record;

    fn cid(prefix: &str) -> CantonId {
        CantonId::parse(&format!(
            "{prefix}::1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892"
        ))
        .unwrap()
    }

    #[test]
    fn confirm_vault_action_builds_the_exercise() {
        let rules = TemplateId::new(
            "#bitsafe-vault-governance-v0-rc8",
            "BitsafeVault.VaultGovernance",
            "VaultGovernanceRules",
        );
        let action = ActionType::GovernanceSetThreshold { new_threshold: 2 };
        let commands = build_confirm_vault_action(
            &rules,
            "00rules",
            &cid("member"),
            &cid("gov"),
            &action,
            "cmd-1".into(),
        )
        .unwrap();
        let Some(command::Command::Exercise(ex)) = &commands.commands[0].command else {
            panic!("expected Exercise")
        };
        assert_eq!(ex.choice, "VaultGovernanceRules_ConfirmAction");
        assert_eq!(ex.contract_id, "00rules");
        let record = extract_record(ex.choice_argument.as_ref().unwrap()).unwrap();
        assert_eq!(record.fields[0].label, "confirmer");
        assert_eq!(record.fields[1].label, "action");
        // a self-only action errors instead of panicking:
        let self_only = ActionType::GovernanceAddAdditionalProposer {
            additional_proposer: cid("p"),
        };
        assert!(
            build_confirm_vault_action(
                &rules,
                "00rules",
                &cid("member"),
                &cid("gov"),
                &self_only,
                "cmd-2".into(),
            )
            .is_err()
        );
    }

    #[test]
    fn cancel_proposal_bundles_both_archives_atomically() {
        let interface = TemplateId::new(
            "#governance-action-v1",
            "Governance.Action",
            "GovernableAction",
        );
        let conf_template = TemplateId::new(
            "#governance-core-v1",
            "Governance.Confirmation",
            "GovernanceConfirmation",
        );
        let commands = build_cancel_proposal(
            &interface,
            "00prop",
            Some(("00conf", &conf_template)),
            &cid("member"),
            &cid("gov"),
            "cmd-3".into(),
        );
        assert_eq!(commands.commands.len(), 2);

        // command[0]: GovernableAction_ProposerCancel on 00prop via the interface id
        let Some(command::Command::Exercise(first)) = &commands.commands[0].command else {
            panic!("expected Exercise")
        };
        assert_eq!(first.choice, "GovernableAction_ProposerCancel");
        assert_eq!(first.contract_id, "00prop");
        assert_eq!(
            first.template_id.as_ref().unwrap().entity_name,
            "GovernableAction"
        );
        assert!(
            extract_record(first.choice_argument.as_ref().unwrap())
                .unwrap()
                .fields
                .is_empty()
        );

        // command[1]: GovernanceConfirmation_Cancel on 00conf
        let Some(command::Command::Exercise(second)) = &commands.commands[1].command else {
            panic!("expected Exercise")
        };
        assert_eq!(second.choice, "GovernanceConfirmation_Cancel");
        assert_eq!(second.contract_id, "00conf");
        assert!(
            extract_record(second.choice_argument.as_ref().unwrap())
                .unwrap()
                .fields
                .is_empty()
        );

        // without own_confirmation, len == 1
        let solo = build_cancel_proposal(
            &interface,
            "00prop",
            None,
            &cid("member"),
            &cid("gov"),
            "cmd-4".into(),
        );
        assert_eq!(solo.commands.len(), 1);
    }

    /// One compact loop covering all thirteen builders: every output must
    /// carry the same `act_as`/`read_as`/`command_id`, and its first
    /// command must exercise the choice the table names.
    #[test]
    fn every_builder_sets_act_as_read_as_command_id_and_choice() {
        let member = cid("member");
        let gov = cid("gov");
        let command_id = "cmd-x";

        let vault_rules = TemplateId::new(
            "#vault-governance-v1",
            "BitsafeVault.VaultGovernance",
            "VaultGovernanceRules",
        );
        let core_rules =
            TemplateId::new("#governance-core-v1", "Governance.Rules", "GovernanceRules");
        let vault_confirmation = TemplateId::new(
            "#vault-governance-v1",
            "BitsafeVault.VaultGovernance",
            "VaultGovernanceConfirmation",
        );
        let self_confirmation = TemplateId::new(
            "#governance-core-v1",
            "Governance.Rules",
            "GovernanceSelfConfirmation",
        );
        let domain_confirmation = TemplateId::new(
            "#governance-core-v1",
            "Governance.Confirmation",
            "GovernanceConfirmation",
        );
        let interface = TemplateId::new(
            "#governance-action-v1",
            "Governance.Action",
            "GovernableAction",
        );
        let action = ActionType::GovernanceSetThreshold { new_threshold: 2 };
        let no_confirmations: &[String] = &[];

        let cases: Vec<(&str, Commands)> = vec![
            (
                "VaultGovernanceRules_ConfirmAction",
                build_confirm_vault_action(
                    &vault_rules,
                    "00rules",
                    &member,
                    &gov,
                    &action,
                    command_id.into(),
                )
                .unwrap(),
            ),
            (
                "GovernanceRules_ConfirmGovernanceAction",
                build_confirm_self_action(
                    &core_rules,
                    "00rules",
                    &member,
                    &gov,
                    &action,
                    command_id.into(),
                )
                .unwrap(),
            ),
            (
                "GovernanceRules_ConfirmAction",
                build_confirm_proposal(
                    &core_rules,
                    "00rules",
                    &member,
                    &gov,
                    "00prop",
                    command_id.into(),
                ),
            ),
            (
                "VaultGovernanceRules_ExecuteConfirmedAction",
                build_execute_vault_action(
                    &vault_rules,
                    "00rules",
                    &member,
                    &gov,
                    &action,
                    no_confirmations,
                    None,
                    vec![],
                    command_id.into(),
                )
                .unwrap(),
            ),
            (
                "GovernanceRules_ExecuteGovernanceAction",
                build_execute_self_action(
                    &core_rules,
                    "00rules",
                    &member,
                    &gov,
                    &action,
                    no_confirmations,
                    vec![],
                    command_id.into(),
                )
                .unwrap(),
            ),
            (
                "GovernanceRules_ExecuteConfirmedAction",
                build_execute_proposal(
                    &core_rules,
                    "00rules",
                    &member,
                    &gov,
                    "00prop",
                    no_confirmations,
                    vec![],
                    command_id.into(),
                ),
            ),
            (
                "VaultGovernanceRules_ExpireConfirmation",
                build_expire_vault_confirmation(
                    &vault_rules,
                    "00rules",
                    &member,
                    &gov,
                    "00conf",
                    command_id.into(),
                ),
            ),
            (
                "GovernanceRules_ExpireGovernanceSelfConfirmation",
                build_expire_self_confirmation(
                    &core_rules,
                    "00rules",
                    &member,
                    &gov,
                    "00conf",
                    command_id.into(),
                ),
            ),
            (
                "GovernanceRules_ExpireConfirmation",
                build_expire_domain_confirmation(
                    &core_rules,
                    "00rules",
                    &member,
                    &gov,
                    "00conf",
                    command_id.into(),
                ),
            ),
            (
                "VaultGovernanceConfirmation_Cancel",
                build_cancel_vault_confirmation(
                    &vault_confirmation,
                    "00conf",
                    &member,
                    &gov,
                    command_id.into(),
                ),
            ),
            (
                "GovernanceSelfConfirmation_Cancel",
                build_cancel_self_confirmation(
                    &self_confirmation,
                    "00conf",
                    &member,
                    &gov,
                    command_id.into(),
                ),
            ),
            (
                "GovernanceConfirmation_Cancel",
                build_cancel_domain_confirmation(
                    &domain_confirmation,
                    "00conf",
                    &member,
                    &gov,
                    command_id.into(),
                ),
            ),
            (
                "GovernableAction_ProposerCancel",
                build_cancel_proposal(&interface, "00prop", None, &member, &gov, command_id.into()),
            ),
        ];

        for (expected_choice, commands) in cases {
            assert_eq!(
                commands.act_as,
                vec![member.to_string()],
                "act_as for {expected_choice}"
            );
            assert_eq!(
                commands.read_as,
                vec![gov.to_string()],
                "read_as for {expected_choice}"
            );
            assert_eq!(
                commands.command_id, command_id,
                "command_id for {expected_choice}"
            );
            let Some(command::Command::Exercise(ex)) = &commands.commands[0].command else {
                panic!("expected Exercise for {expected_choice}")
            };
            assert_eq!(ex.choice, expected_choice);
        }
    }
}
