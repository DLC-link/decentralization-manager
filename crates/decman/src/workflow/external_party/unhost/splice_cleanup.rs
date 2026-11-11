use anyhow::Context;
use canton_proto_rs::com::daml::ledger::api::v2::{
    Command, Commands, CreatedEvent, ExerciseCommand, Identifier, Record, RecordField,
    SubmitAndWaitRequest, Value, command, value,
};
use decman_lib::framework::{
    filters::{party_event_format, template_filter},
    record::{field_party, field_timestamp},
};

use super::attempt::{Progress, UnhostEnv};
use crate::{
    canton_id::CantonId,
    config::NodeConfig,
    server::ledger_paging::fetch_active_contracts_filtered,
    utils,
    workflow::external_party::unhost_audit::{self},
};

/// The Splice templates are named by package so Canton matches the contracts of
/// every vetted `splice-amulet` version, not just one package id.
const SPLICE_AMULET: &str = "#splice-amulet";

fn validator_right_template() -> Identifier {
    Identifier {
        package_id: SPLICE_AMULET.to_string(),
        module_name: "Splice.Amulet".to_string(),
        entity_name: "ValidatorRight".to_string(),
    }
}

fn transfer_preapproval_template() -> Identifier {
    Identifier {
        package_id: SPLICE_AMULET.to_string(),
        module_name: "Splice.AmuletRules".to_string(),
        entity_name: "TransferPreapproval".to_string(),
    }
}

/// A `TransferPreapproval` the operator provides for the party.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Preapproval {
    pub(super) contract_id: String,
    /// Microseconds since the epoch, as Daml stores `Time`.
    pub(super) expires_at: i64,
}

/// The party's Splice contracts that the operator holds.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct SpliceContracts {
    pub(super) validator_rights: Vec<String>,
    pub(super) preapprovals: Vec<Preapproval>,
}

enum SpliceContract {
    ValidatorRight(String),
    Preapproval(Preapproval),
}

/// Keep a contract only when it ties `party` to `operator`: a `ValidatorRight`
/// with `user == party` and `validator == operator`, or a `TransferPreapproval`
/// with `receiver == party` and `provider == operator`.
///
/// The operator sees these contracts for every user it serves, so the filter
/// is what keeps the cleanup to the party leaving.
fn classify(created: &CreatedEvent, party: &str, operator: &str) -> Option<SpliceContract> {
    let template = created.template_id.as_ref()?;
    let args = created.create_arguments.as_ref()?;
    let is = |label: &str, want: &str| field_party(args, label).as_deref() == Some(want);
    match template.entity_name.as_str() {
        "ValidatorRight" if is("user", party) && is("validator", operator) => {
            Some(SpliceContract::ValidatorRight(created.contract_id.clone()))
        }
        "TransferPreapproval" if is("receiver", party) && is("provider", operator) => {
            Some(SpliceContract::Preapproval(Preapproval {
                contract_id: created.contract_id.clone(),
                expires_at: field_timestamp(args, "expiresAt")?,
            }))
        }
        _ => None,
    }
}

/// Read the party's `ValidatorRight` and `TransferPreapproval` contracts as the
/// operator, which is a signatory on both and so sees them without any right
/// over the party.
pub(super) async fn find_splice_contracts(
    config: &NodeConfig,
    party_id: &str,
    operator: &CantonId,
    token: &str,
) -> anyhow::Result<SpliceContracts> {
    let format = party_event_format(
        operator,
        vec![
            template_filter(validator_right_template(), false),
            template_filter(transfer_preapproval_template(), false),
        ],
        true,
    );
    let operator_id = operator.to_string();
    let found = match fetch_active_contracts_filtered(
        config,
        Some(token.to_string()),
        format,
        |c| classify(&c, party_id, &operator_id),
    )
    .await
    {
        Ok(found) => found,
        // A participant without splice-amulet has no such contracts to archive.
        Err(e) if format!("{e:#}").contains("NO_TEMPLATES_FOR_PACKAGE_NAME_AND_QUALIFIED_NAME") => {
            tracing::info!(
                %party_id,
                "external-party unhost: splice-amulet is not on this participant; nothing to archive"
            );
            Vec::new()
        }
        Err(e) => return Err(e.context("reading the party's Splice contracts as the operator")),
    };

    let mut contracts = SpliceContracts::default();
    for contract in found {
        match contract {
            SpliceContract::ValidatorRight(cid) => contracts.validator_rights.push(cid),
            SpliceContract::Preapproval(p) => contracts.preapprovals.push(p),
        }
    }
    Ok(contracts)
}

pub(super) async fn operator_token(
    env: &UnhostEnv<'_>,
    operator: &CantonId,
) -> anyhow::Result<String> {
    let auth = env
        .auth
        .context("this host has no party credentials to act as its validator operator party")?;
    Ok(auth
        .get_credentials(operator)
        .await
        .with_context(|| format!("minting a ledger token for the validator operator {operator}"))?
        .token)
}

/// Exercise `choice` on one contract as the operator, and wait for the result.
async fn exercise_as_operator(
    config: &NodeConfig,
    token: &str,
    operator: &CantonId,
    template_id: Identifier,
    contract_id: &str,
    choice: &str,
    argument: Record,
) -> anyhow::Result<()> {
    let mut client = utils::create_command_client(config, Some(token.to_string())).await?;
    let commands = Commands {
        command_id: uuid::Uuid::new_v4().to_string(),
        act_as: vec![operator.to_string()],
        commands: vec![Command {
            command: Some(command::Command::Exercise(ExerciseCommand {
                template_id: Some(template_id),
                contract_id: contract_id.to_string(),
                choice: choice.to_string(),
                choice_argument: Some(Value {
                    sum: Some(value::Sum::Record(argument)),
                }),
            })),
        }],
        ..Default::default()
    };
    client
        .submit_and_wait(tonic::Request::new(SubmitAndWaitRequest {
            commands: Some(commands),
        }))
        .await
        .with_context(|| format!("{choice} on {contract_id}"))?;
    Ok(())
}

/// Archive every `ValidatorRight` and cancel every live `TransferPreapproval`
/// the operator holds for the party. Each archive is written to the audit row
/// as it happens.
pub(super) async fn splice_cleanup(
    env: &UnhostEnv<'_>,
    party_id: &str,
    operator: &CantonId,
    progress: &mut Progress,
) -> anyhow::Result<()> {
    let token = operator_token(env, operator).await?;
    let found = find_splice_contracts(env.config, party_id, operator, &token).await?;
    if found == SpliceContracts::default() {
        tracing::info!(
            %party_id,
            "external-party unhost: this host holds no Splice contracts for the party"
        );
    }

    for cid in found.validator_rights {
        exercise_as_operator(
            env.config,
            &token,
            operator,
            validator_right_template(),
            &cid,
            "ValidatorRight_ArchiveAsValidator",
            Record::default(),
        )
        .await?;
        progress.validator_rights_archived.push(cid);
        save_cleanup(env, progress).await?;
    }

    for preapproval in found.preapprovals {
        // Past `expiresAt` the cancel choice refuses, and the DSO expires the
        // contract itself, so an expired one counts as done.
        if preapproval.expires_at <= now_micros() {
            tracing::info!(
                %party_id,
                contract_id = %preapproval.contract_id,
                "external-party unhost: TransferPreapproval already expired; leaving it to the DSO"
            );
            continue;
        }
        let cancelled = exercise_as_operator(
            env.config,
            &token,
            operator,
            transfer_preapproval_template(),
            &preapproval.contract_id,
            "TransferPreapproval_Cancel",
            Record {
                record_id: None,
                fields: vec![RecordField {
                    label: "p".to_string(),
                    value: Some(Value {
                        sum: Some(value::Sum::Party(operator.to_string())),
                    }),
                }],
            },
        )
        .await;
        match cancelled {
            Ok(()) => {
                progress
                    .preapprovals_cancelled
                    .push(preapproval.contract_id);
                save_cleanup(env, progress).await?;
            }
            // It expired between the read and the cancel.
            Err(e) if preapproval.expires_at <= now_micros() => {
                tracing::info!(
                    %party_id,
                    contract_id = %preapproval.contract_id,
                    "external-party unhost: TransferPreapproval expired during the cancel: {e:#}"
                );
            }
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

async fn save_cleanup(env: &UnhostEnv<'_>, progress: &Progress) -> anyhow::Result<()> {
    unhost_audit::set_cleanup(
        env.db,
        progress.audit_id,
        &progress.validator_rights_archived,
        &progress.preapprovals_cancelled,
    )
    .await
}

pub(super) fn now_micros() -> i64 {
    chrono::Utc::now().timestamp_micros()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::external_party::unhost::test_support::*;

    fn party_field(label: &str, party: &str) -> RecordField {
        RecordField {
            label: label.to_string(),
            value: Some(Value {
                sum: Some(value::Sum::Party(party.to_string())),
            }),
        }
    }

    fn created(entity: &str, fields: Vec<RecordField>) -> CreatedEvent {
        CreatedEvent {
            contract_id: format!("{entity}-cid"),
            template_id: Some(Identifier {
                package_id: "pkg".to_string(),
                module_name: "Splice.Amulet".to_string(),
                entity_name: entity.to_string(),
            }),
            create_arguments: Some(Record {
                record_id: None,
                fields,
            }),
            ..Default::default()
        }
    }

    fn validator_right(user: &str, validator: &str) -> CreatedEvent {
        created(
            "ValidatorRight",
            vec![
                party_field("dso", "dso::1220ee"),
                party_field("user", user),
                party_field("validator", validator),
            ],
        )
    }

    fn preapproval(receiver: &str, provider: &str) -> CreatedEvent {
        let mut event = created(
            "TransferPreapproval",
            vec![
                party_field("receiver", receiver),
                party_field("provider", provider),
            ],
        );
        if let Some(args) = event.create_arguments.as_mut() {
            args.fields.push(RecordField {
                label: "expiresAt".to_string(),
                value: Some(Value {
                    sum: Some(value::Sum::Timestamp(1_700_000_000_000_000)),
                }),
            });
        }
        event
    }

    /// The operator sees these contracts for every user it serves. Only the
    /// ones tying the leaving party to this operator may be archived.
    #[test]
    fn only_the_partys_contracts_with_this_operator_are_kept() {
        assert!(matches!(
            classify(&validator_right(PARTY, OPERATOR), PARTY, OPERATOR),
            Some(SpliceContract::ValidatorRight(cid)) if cid == "ValidatorRight-cid"
        ));
        assert!(classify(&validator_right("bob::1220ff", OPERATOR), PARTY, OPERATOR).is_none());
        assert!(classify(&validator_right(PARTY, "other::1220ff"), PARTY, OPERATOR).is_none());

        assert!(matches!(
            classify(&preapproval(PARTY, OPERATOR), PARTY, OPERATOR),
            Some(SpliceContract::Preapproval(p)) if p.expires_at == 1_700_000_000_000_000
        ));
        assert!(classify(&preapproval(PARTY, "other::1220ff"), PARTY, OPERATOR).is_none());
        assert!(classify(&preapproval("bob::1220ff", OPERATOR), PARTY, OPERATOR).is_none());
    }

    #[test]
    fn other_templates_are_ignored() {
        let event = created(
            "FeaturedAppRight",
            vec![
                party_field("user", PARTY),
                party_field("validator", OPERATOR),
            ],
        );
        assert!(classify(&event, PARTY, OPERATOR).is_none());
    }
}
