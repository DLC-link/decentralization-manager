//! The `Commands` envelope, proposal create-arguments, and the `build_propose`
//! helper every `GovernableAction` payload shares, plus the `Transaction`
//! contract-id lookup the propose flow uses to read its new proposal's cid.

use canton_proto_rs::com::daml::ledger::api::v2::{
    Command, Commands, CreateCommand, DisclosedContract, Record, Transaction, command, event, value,
};
use common::{api::PackageConfig, canton_id::CantonId};

use crate::error::Error;
use crate::framework::encode::{field, make_party};
use crate::framework::{DamlProtoEncode, TemplateInfo};

/// The one `Commands` envelope every governance submission uses.
/// `member` acts (`act_as`); the decentralized party reads (`read_as`).
pub fn commands_envelope(
    command_id: String,
    member: &CantonId,
    governance_party: &CantonId,
    commands: Vec<Command>,
    disclosed_contracts: Vec<DisclosedContract>,
) -> Commands {
    Commands {
        workflow_id: String::new(),
        user_id: String::new(),
        command_id,
        commands,
        deduplication_period: None,
        min_ledger_time_abs: None,
        min_ledger_time_rel: None,
        act_as: vec![member.to_string()],
        read_as: vec![governance_party.to_string()],
        submission_id: String::new(),
        disclosed_contracts,
        synchronizer_id: String::new(),
        package_id_selection_preference: vec![],
        prefetch_contract_keys: vec![],
        taps_max_passes: None,
    }
}

/// The create-arguments record for a proposal: the payload's own fields,
/// with `governanceParty` and `proposer` injected first.
pub fn proposal_create_arguments(
    payload: &(impl DamlProtoEncode + ?Sized),
    governance_party: &CantonId,
    proposer: &CantonId,
) -> Result<Record, Error> {
    let encoded = payload.to_daml_proto()?;
    let Some(value::Sum::Record(record)) = encoded.sum else {
        return Err(Error::Encode(
            "proposal payload must encode to a Record".into(),
        ));
    };
    let mut fields = vec![
        field("governanceParty", make_party(governance_party)),
        field("proposer", make_party(proposer)),
    ];
    fields.extend(record.fields);
    Ok(Record {
        record_id: None,
        fields,
    })
}

/// A complete propose submission for any `GovernableAction` payload.
///
/// `?Sized` so a caller holding an erased payload — decman's
/// `ProposalType::grpc_payload`, which hands back a
/// `&dyn GrpcPayload` rather than re-matching 29 variants — can submit
/// through the same entry point as a concrete struct. `dyn GrpcPayload`
/// implements its supertraits automatically, so nothing else is needed.
pub fn build_propose(
    payload: &(impl TemplateInfo + DamlProtoEncode + ?Sized),
    governance_party: &CantonId,
    proposer: &CantonId,
    packages: &PackageConfig,
    command_id: String,
) -> Result<Commands, Error> {
    let template = payload.template_id(packages)?;
    let create_arguments = proposal_create_arguments(payload, governance_party, proposer)?;
    let cmd = Command {
        command: Some(command::Command::Create(CreateCommand {
            template_id: Some((&template).into()),
            create_arguments: Some(create_arguments),
        })),
    };
    Ok(commands_envelope(
        command_id,
        proposer,
        governance_party,
        vec![cmd],
        vec![],
    ))
}

/// The contract id of the first created event in a transaction. The
/// propose flow reads its new proposal's cid with this.
pub fn first_created_contract_id(transaction: &Transaction) -> Option<String> {
    transaction
        .events
        .iter()
        .find_map(|e| match e.event.as_ref() {
            Some(event::Event::Created(created)) => Some(created.contract_id.clone()),
            _ => None,
        })
}

#[cfg(test)]
mod tests {
    use canton_proto_rs::com::daml::ledger::api::v2::{ArchivedEvent, CreatedEvent, Event, Value};

    use super::*;
    use crate::framework::encode::make_text;
    use crate::framework::traits::tests::FakeProposal;

    fn cid(prefix: &str) -> CantonId {
        CantonId::parse(&format!(
            "{prefix}::1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892"
        ))
        .unwrap()
    }

    fn test_packages() -> PackageConfig {
        PackageConfig {
            governance_action: None,
            governance_core: None,
            governance_rewards: None,
            governance_token_custody: None,
            governance_utility_credential: None,
            governance_utility_onboarding: None,
            utility_credential: None,
            utility_credential_app: None,
            utility_registry: None,
            vault: None,
            vault_governance: None,
        }
    }

    #[test]
    fn envelope_sets_actor_reader_and_id() {
        let c = commands_envelope("cmd-1".into(), &cid("member"), &cid("gov"), vec![], vec![]);
        assert_eq!(c.command_id, "cmd-1");
        assert_eq!(c.act_as, vec![cid("member").to_string()]);
        assert_eq!(c.read_as, vec![cid("gov").to_string()]);
        assert!(c.disclosed_contracts.is_empty());
    }

    #[test]
    fn build_propose_injects_the_party_fields_first() {
        let p = FakeProposal { note: "x".into() };
        let commands = build_propose(
            &p,
            &cid("gov"),
            &cid("member"),
            &test_packages(),
            "cmd-2".into(),
        )
        .unwrap();
        let Some(command::Command::Create(create)) = &commands.commands[0].command else {
            panic!("expected a CreateCommand");
        };
        let labels: Vec<&str> = create
            .create_arguments
            .as_ref()
            .unwrap()
            .fields
            .iter()
            .map(|f| f.label.as_str())
            .collect();
        assert_eq!(labels, vec!["governanceParty", "proposer", "note"]);
        let template = create.template_id.as_ref().unwrap();
        assert_eq!(template.package_id, "#fake-pkg");
    }

    #[test]
    fn non_record_payload_is_an_encode_error() {
        struct Bad;
        impl DamlProtoEncode for Bad {
            fn to_daml_proto(&self) -> Result<Value, Error> {
                Ok(make_text("no"))
            }
        }
        let err = proposal_create_arguments(&Bad, &cid("gov"), &cid("member")).unwrap_err();
        assert!(matches!(err, Error::Encode(_)));
    }

    #[test]
    fn first_created_contract_id_finds_the_first_create() {
        let transaction = Transaction {
            events: vec![
                Event {
                    event: Some(event::Event::Archived(ArchivedEvent {
                        contract_id: "archived-cid".into(),
                        ..Default::default()
                    })),
                },
                Event {
                    event: Some(event::Event::Created(CreatedEvent {
                        contract_id: "created-cid".into(),
                        ..Default::default()
                    })),
                },
            ],
            ..Default::default()
        };
        assert_eq!(
            first_created_contract_id(&transaction),
            Some("created-cid".to_string())
        );
        assert_eq!(first_created_contract_id(&Transaction::default()), None);
    }
}
