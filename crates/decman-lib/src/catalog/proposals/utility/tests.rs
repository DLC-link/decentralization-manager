//! Encode snapshots and validation tests for the utility proposals.
//!
//! They stay in one module so the snapshot files keep their names, which
//! insta derives from the module path.

use common::{
    api::{
        InstrumentId, InstrumentIdentifier, InstrumentIssuerCredentials,
        PartyCredentialRequirement, RequiredClaim,
    },
    canton_id::CantonId,
};

use super::*;
use crate::{
    catalog::types::AppRewardBeneficiary,
    error::Error,
    framework::{
        DamlProtoEncode, TemplateInfo, Validate, ValidationCtx,
        commands::proposal_create_arguments,
        encode::{
            field, make_contract_id, make_optional_bool, make_optional_list, make_optional_text,
            make_optional_timestamp,
        },
    },
};

/// Where the encode snapshots live, relative to this file. The file sits one
/// directory below `catalog/proposals/`, so the path has one more `..` than
/// `catalog::proposals::SNAPSHOT_PATH`.
const SNAPSHOT_PATH: &str = "../../../../snapshots";

/// Any valid `CantonId` — the exact value is irrelevant to these
/// encode-shape snapshots.
fn cid(prefix: &str) -> CantonId {
    let ns = "1220c4010d6883f367c7f45d55b2449501620130f9b21e96379f17dea455ac7a5892";
    CantonId::parse(&format!("{prefix}::{ns}")).expect("valid canton id")
}

fn ctx(governance_party: &CantonId, now_micros: i64) -> ValidationCtx<'_> {
    ValidationCtx {
        governance_party,
        now_micros,
    }
}

fn requirement(issuer: &CantonId, claims: &[(&str, &str)]) -> PartyCredentialRequirement {
    PartyCredentialRequirement {
        issuer: issuer.clone(),
        required_claims: claims
            .iter()
            .map(|(property, value)| RequiredClaim {
                property: property.to_string(),
                value: value.to_string(),
            })
            .collect(),
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "The test pins one snapshot per payload, so it grows with the payload list."
)]
fn encode_snapshots() {
    let mut settings = insta::Settings::clone_current();
    settings.set_snapshot_path(SNAPSHOT_PATH);
    let _guard = settings.bind_to_scope();

    insta::assert_debug_snapshot!(
        "provision_provider_service",
        ProvisionProviderService {}
            .to_daml_proto()
            .expect("payload encodes")
    );
    insta::assert_debug_snapshot!(
        "create_provider_service_request",
        CreateProviderServiceRequest {
            operator: cid("op"),
            provider: cid("prov"),
        }
        .to_daml_proto()
        .expect("payload encodes")
    );
    insta::assert_debug_snapshot!(
        "create_user_service_request",
        CreateUserServiceRequest {
            operator: cid("op"),
            user: cid("user"),
        }
        .to_daml_proto()
        .expect("payload encodes")
    );
    insta::assert_debug_snapshot!(
        "create_delegated_batched_markers_proxy",
        CreateDelegatedBatchedMarkersProxy {
            operator: cid("op")
        }
        .to_daml_proto()
        .expect("payload encodes")
    );
    insta::assert_debug_snapshot!(
        "request_dev_net_featured_app_right",
        RequestDevNetFeaturedAppRight {
            amulet_rules_cid: "00amulet".to_string(),
        }
        .to_daml_proto()
        .expect("payload encodes")
    );
    insta::assert_debug_snapshot!(
        "setup_utility",
        SetupUtility {
            provider_service_cid: "00psc".to_string(),
            operator: cid("op"),
            instrument_id_text: "uuid-1".to_string(),
            additional_identifiers: vec![InstrumentIdentifier {
                source: cid("src"),
                id: "TICK".to_string(),
                scheme: "Ticker".to_string(),
            }],
            create_transfer_rule: true,
            create_allocation_factory: false,
            use_block_list: Some(true),
        }
        .to_daml_proto()
        .expect("payload encodes")
    );
    insta::assert_debug_snapshot!(
        "set_provider_app_reward_beneficiaries_some",
        SetProviderAppRewardBeneficiaries {
            instrument_configuration_cid: "00icc".to_string(),
            provider_app_reward_beneficiaries: Some(vec![AppRewardBeneficiary {
                beneficiary: cid("b1"),
                weight: "1.0".parse().expect("valid decimal"),
            }]),
        }
        .to_daml_proto()
        .expect("payload encodes")
    );
    insta::assert_debug_snapshot!(
        "set_provider_app_reward_beneficiaries_none",
        SetProviderAppRewardBeneficiaries {
            instrument_configuration_cid: "00icc".to_string(),
            provider_app_reward_beneficiaries: None,
        }
        .to_daml_proto()
        .expect("payload encodes")
    );
    insta::assert_debug_snapshot!(
        "set_enable_result_contracts_some",
        SetEnableResultContracts {
            registrar_service_cid: "00rsc".to_string(),
            enable_result_contracts: Some(true),
        }
        .to_daml_proto()
        .expect("payload encodes")
    );
    insta::assert_debug_snapshot!(
        "set_enable_result_contracts_none",
        SetEnableResultContracts {
            registrar_service_cid: "00rsc".to_string(),
            enable_result_contracts: None,
        }
        .to_daml_proto()
        .expect("payload encodes")
    );
    insta::assert_debug_snapshot!(
        "create_transfer_rule",
        CreateTransferRule {
            registrar_service_cid: "00rsc".to_string(),
        }
        .to_daml_proto()
        .expect("payload encodes")
    );
    insta::assert_debug_snapshot!(
        "mint",
        Mint {
            allocation_factory_cid: "00alloc".to_string(),
            instrument_id: InstrumentId {
                admin: "admin::ns".to_string(),
                id: "instr-1".to_string(),
            },
            instrument_configuration_cid: "00icc".to_string(),
            recipient: cid("recv"),
            amount: "1.5".parse().expect("valid decimal"),
            description: "mint it".to_string(),
            block_list_shard_cids: vec!["00shard1".to_string(), "00shard2".to_string()],
        }
        .to_daml_proto()
        .expect("payload encodes")
    );
    insta::assert_debug_snapshot!(
        "burn",
        Burn {
            allocation_factory_cid: "00alloc".to_string(),
            instrument_id: InstrumentId {
                admin: "admin::ns".to_string(),
                id: "instr-1".to_string(),
            },
            instrument_configuration_cid: "00icc".to_string(),
            holder: cid("holder"),
            amount: "1.5".parse().expect("valid decimal"),
            description: "burn it".to_string(),
            block_list_shard_cids: vec!["00shard1".to_string(), "00shard2".to_string()],
        }
        .to_daml_proto()
        .expect("payload encodes")
    );
    insta::assert_debug_snapshot!(
        "accept_mint_request_some",
        AcceptMintRequest {
            mint_request_cid: "00mrc".to_string(),
            instrument_configuration_cid: "00icc".to_string(),
            issuer_credential_cids: vec!["00cred1".to_string(), "00cred2".to_string()],
            description: "accept mint".to_string(),
            block_list_shard_cids: vec!["00shard1".to_string(), "00shard2".to_string()],
        }
        .to_daml_proto()
        .expect("payload encodes")
    );
    insta::assert_debug_snapshot!(
        "accept_mint_request_none",
        AcceptMintRequest {
            mint_request_cid: "00mrc".to_string(),
            instrument_configuration_cid: "00icc".to_string(),
            issuer_credential_cids: vec![],
            description: "accept mint".to_string(),
            block_list_shard_cids: vec![],
        }
        .to_daml_proto()
        .expect("payload encodes")
    );
    insta::assert_debug_snapshot!(
        "accept_burn_request_some",
        AcceptBurnRequest {
            burn_request_cid: "00brc".to_string(),
            instrument_configuration_cid: "00icc".to_string(),
            issuer_credential_cids: vec!["00cred1".to_string(), "00cred2".to_string()],
            description: "accept burn".to_string(),
            block_list_shard_cids: vec!["00shard1".to_string(), "00shard2".to_string()],
        }
        .to_daml_proto()
        .expect("payload encodes")
    );
    insta::assert_debug_snapshot!(
        "accept_burn_request_none",
        AcceptBurnRequest {
            burn_request_cid: "00brc".to_string(),
            instrument_configuration_cid: "00icc".to_string(),
            issuer_credential_cids: vec![],
            description: "accept burn".to_string(),
            block_list_shard_cids: vec![],
        }
        .to_daml_proto()
        .expect("payload encodes")
    );
    insta::assert_debug_snapshot!(
        "create_provider_configuration",
        CreateProviderConfiguration {
            provider_service_cid: "00psc".to_string(),
            registrar_requirements: vec![requirement(
                &cid("issuer"),
                &[("role", "registrar"), ("kyc", "passed")],
            )],
            holder_requirements: vec![requirement(&cid("issuer"), &[("role", "holder")])],
        }
        .to_daml_proto()
        .expect("payload encodes")
    );
    insta::assert_debug_snapshot!(
        "create_registrar_service_request",
        CreateRegistrarServiceRequest {
            operator: cid("op"),
            provider: cid("prov"),
            create_transfer_rule: true,
            create_allocation_factory: false,
        }
        .to_daml_proto()
        .expect("payload encodes")
    );
    insta::assert_debug_snapshot!(
        "onboard_registrar",
        OnboardRegistrar {
            provider_service_cid: "00psc".to_string(),
            registrar_service_request_cid: "00rsrc".to_string(),
            provider_configuration_cid: "00pcc".to_string(),
        }
        .to_daml_proto()
        .expect("payload encodes")
    );
    insta::assert_debug_snapshot!(
        "provision_instrument_populated",
        ProvisionInstrument {
            registrar_service_cid: "00rsc".to_string(),
            instrument_id_text: "uuid-1".to_string(),
            additional_identifiers: vec![InstrumentIdentifier {
                source: cid("src"),
                id: "TICK".to_string(),
                scheme: "Ticker".to_string(),
            }],
            issuer_requirements: vec![requirement(
                &cid("issuer"),
                &[("role", "instrument-issuer")]
            )],
            holder_requirements: vec![requirement(&cid("issuer"), &[("role", "holder")])],
            initial_instrument_issuers: vec![cid("issuer")],
            credential_valid_from_micros: None,
            credential_valid_until_micros: None,
            use_block_list: None,
        }
        .to_daml_proto()
        .expect("payload encodes")
    );
    insta::assert_debug_snapshot!(
        "provision_instrument_empty",
        ProvisionInstrument {
            registrar_service_cid: "00rsc".to_string(),
            instrument_id_text: "uuid-1".to_string(),
            additional_identifiers: vec![],
            issuer_requirements: vec![],
            holder_requirements: vec![],
            initial_instrument_issuers: vec![],
            credential_valid_from_micros: None,
            credential_valid_until_micros: None,
            use_block_list: None,
        }
        .to_daml_proto()
        .expect("payload encodes")
    );
    insta::assert_debug_snapshot!(
        "update_instrument_configuration",
        UpdateInstrumentConfiguration {
            registrar_service_cid: "00rsc".to_string(),
            instrument_configuration_cid: "00icc".to_string(),
            instrument_id_text: "uuid-1".to_string(),
            additional_identifiers: vec![InstrumentIdentifier {
                source: cid("src"),
                id: "TICK".to_string(),
                scheme: "Ticker".to_string(),
            }],
            issuer_requirements: vec![requirement(
                &cid("issuer"),
                &[("role", "instrument-issuer")]
            )],
            holder_requirements: vec![requirement(&cid("issuer"), &[("role", "holder")])],
            use_block_list: Some(true),
        }
        .to_daml_proto()
        .expect("payload encodes")
    );
    insta::assert_debug_snapshot!(
        "onboard_instrument_issuers",
        OnboardInstrumentIssuers {
            instrument_configuration_cid: "00icc".to_string(),
            instrument_issuers: vec![cid("iss1"), cid("iss2")],
            credential_valid_from_micros: None,
            credential_valid_until_micros: None,
        }
        .to_daml_proto()
        .expect("payload encodes")
    );
    insta::assert_debug_snapshot!(
        "offboard_instrument_issuers",
        OffboardInstrumentIssuers {
            instrument_issuers: vec![InstrumentIssuerCredentials {
                instrument_issuer: cid("iss1"),
                credential_cids: vec!["00cred1".to_string(), "00cred2".to_string()],
            }],
        }
        .to_daml_proto()
        .expect("payload encodes")
    );
    insta::assert_debug_snapshot!(
        "onboard_instrument_issuers_with_credential_id_nonce",
        WithCredentialIdNonce {
            proposal: &OnboardInstrumentIssuers {
                instrument_configuration_cid: "00icc".to_string(),
                instrument_issuers: vec![cid("iss1"), cid("iss2")],
                credential_valid_from_micros: None,
                credential_valid_until_micros: None,
            },
            nonce: NONCE,
        }
        .to_daml_proto()
        .expect("payload encodes")
    );
}

/// A fixed nonce keeps the encodings deterministic. The server draws a
/// random one per proposal.
const NONCE: &str = "0123456789ab";

/// One instance of each `MintsCredentials` proposal.
fn credential_minting_proposals() -> Vec<Box<dyn MintsCredentials>> {
    vec![
        Box::new(OnboardRegistrar {
            provider_service_cid: "00psc".to_string(),
            registrar_service_request_cid: "00rsrc".to_string(),
            provider_configuration_cid: "00pcc".to_string(),
        }),
        Box::new(ProvisionInstrument {
            registrar_service_cid: "00rsc".to_string(),
            instrument_id_text: "uuid-1".to_string(),
            additional_identifiers: vec![],
            issuer_requirements: vec![],
            holder_requirements: vec![],
            initial_instrument_issuers: vec![cid("issuer")],
            credential_valid_from_micros: Some(1_700_000_000_000_000),
            credential_valid_until_micros: Some(1_800_000_000_000_000),
            use_block_list: Some(true),
        }),
        Box::new(OnboardInstrumentIssuers {
            instrument_configuration_cid: "00icc".to_string(),
            instrument_issuers: vec![cid("issuer")],
            credential_valid_from_micros: None,
            credential_valid_until_micros: None,
        }),
    ]
}

/// The nonce closes the create arguments, followed by the validity window
/// on the templates that declare one, in the order they declare them.
/// `ProvisionInstrument` then gets `useBlockList`. `OnboardRegistrar`
/// declares none of these, so it gets the nonce alone. Every other field
/// is unchanged.
#[test]
fn credential_id_nonce_and_validity_are_the_last_create_arguments() {
    let gov = cid("gov");
    let member = cid("member");
    for proposal in credential_minting_proposals() {
        let bare = proposal_create_arguments(proposal.as_ref(), &gov, &member)
            .expect("proposal arguments encode");
        let wrapped = proposal_create_arguments(
            &WithCredentialIdNonce {
                proposal: proposal.as_ref(),
                nonce: NONCE,
            },
            &gov,
            &member,
        )
        .expect("proposal arguments encode");
        let window = proposal.credential_validity_micros();
        let after_window = proposal.fields_after_validity();
        let added = if window.is_some() { 3 } else { 1 } + after_window.len();
        let (head, tail) = wrapped.fields.split_at(wrapped.fields.len() - added);
        assert_eq!(head, bare.fields.as_slice(), "other fields unchanged");
        let labels: Vec<&str> = tail.iter().map(|f| f.label.as_str()).collect();
        let mut expected = vec!["credentialIdNonce"];
        if window.is_some() {
            expected.extend(["credentialValidFrom", "credentialValidUntil"]);
        }
        expected.extend(after_window.iter().map(|f| f.label.as_str()));
        assert_eq!(labels, expected);
        assert_eq!(tail[0].value, Some(make_optional_text(Some(NONCE))));
        if let Some((valid_from, valid_until)) = window {
            assert_eq!(tail[1].value, Some(make_optional_timestamp(valid_from)));
            assert_eq!(tail[2].value, Some(make_optional_timestamp(valid_until)));
        }
        assert_eq!(&tail[added - after_window.len()..], after_window.as_slice());
    }
}

/// Only `ProvisionInstrument` declares a field after the window, and it
/// carries the proposal's block list flag.
#[test]
fn provision_instrument_sends_use_block_list_after_the_window() {
    let labels: Vec<Vec<String>> = credential_minting_proposals()
        .iter()
        .map(|p| {
            p.fields_after_validity()
                .into_iter()
                .map(|f| f.label)
                .collect()
        })
        .collect();
    assert_eq!(
        labels,
        vec![vec![], vec!["useBlockList".to_string()], vec![]]
    );
    for flag in [None, Some(false), Some(true)] {
        let proposal = ProvisionInstrument {
            registrar_service_cid: "00rsc".to_string(),
            instrument_id_text: "uuid-1".to_string(),
            additional_identifiers: vec![],
            issuer_requirements: vec![],
            holder_requirements: vec![],
            initial_instrument_issuers: vec![],
            credential_valid_from_micros: None,
            credential_valid_until_micros: None,
            use_block_list: flag,
        };
        assert_eq!(
            proposal.fields_after_validity(),
            vec![field("useBlockList", make_optional_bool(&flag))]
        );
    }
}

/// The accept and offer templates for mint and burn declare
/// `blockListShardCids` last, so Smart Contract Upgrade accepts it. An
/// empty list goes out as `None`, which leaves the choice context as it
/// was.
#[test]
fn mint_and_burn_proposals_send_block_list_shards_last() {
    let gov = cid("gov");
    let member = cid("member");
    let instrument_id = || InstrumentId {
        admin: "admin::ns".to_string(),
        id: "instr-1".to_string(),
    };
    let proposals = |shards: Vec<String>| -> Vec<Box<dyn DamlProtoEncode>> {
        vec![
            Box::new(Mint {
                allocation_factory_cid: "00alloc".to_string(),
                instrument_id: instrument_id(),
                instrument_configuration_cid: "00icc".to_string(),
                recipient: cid("recv"),
                amount: "1.5".parse().expect("valid decimal"),
                description: "mint it".to_string(),
                block_list_shard_cids: shards.clone(),
            }),
            Box::new(Burn {
                allocation_factory_cid: "00alloc".to_string(),
                instrument_id: instrument_id(),
                instrument_configuration_cid: "00icc".to_string(),
                holder: cid("holder"),
                amount: "1.5".parse().expect("valid decimal"),
                description: "burn it".to_string(),
                block_list_shard_cids: shards.clone(),
            }),
            Box::new(AcceptMintRequest {
                mint_request_cid: "00mrc".to_string(),
                instrument_configuration_cid: "00icc".to_string(),
                issuer_credential_cids: vec![],
                description: "accept mint".to_string(),
                block_list_shard_cids: shards.clone(),
            }),
            Box::new(AcceptBurnRequest {
                burn_request_cid: "00brc".to_string(),
                instrument_configuration_cid: "00icc".to_string(),
                issuer_credential_cids: vec![],
                description: "accept burn".to_string(),
                block_list_shard_cids: shards,
            }),
        ]
    };
    let shards = vec!["00shard1".to_string(), "00shard2".to_string()];
    for (cids, expected) in [
        (vec![], make_optional_list(vec![])),
        (
            shards.clone(),
            make_optional_list(shards.iter().map(|c| make_contract_id(c)).collect()),
        ),
    ] {
        for proposal in proposals(cids) {
            let arguments = proposal_create_arguments(proposal.as_ref(), &gov, &member)
                .expect("proposal arguments encode");
            let last = arguments.fields.last().expect("arguments have fields");
            assert_eq!(last.label, "blockListShardCids");
            assert_eq!(last.value, Some(expected.clone()));
        }
    }
}

#[test]
fn credential_id_nonce_keeps_the_proposal_template() {
    for proposal in credential_minting_proposals() {
        let wrapped = WithCredentialIdNonce {
            proposal: proposal.as_ref(),
            nonce: NONCE,
        };
        assert_eq!(
            wrapped.template_id(&"#pkg").expect("template id resolves"),
            proposal.template_id(&"#pkg").expect("template id resolves"),
        );
    }
}

/// Both issuer proposals mirror the templates' window checks: a window
/// that does not start before it ends is refused, and so is one that has
/// already ended, which execution would refuse after the vote.
#[test]
fn minting_proposals_validate_the_credential_validity_window() {
    let gov = cid("gov");
    let now = 1_750_000_000_000_000;
    let with_window = |from: Option<i64>, until: Option<i64>| -> Vec<Box<dyn Validate>> {
        vec![
            Box::new(ProvisionInstrument {
                registrar_service_cid: "00rsc".to_string(),
                instrument_id_text: "uuid-1".to_string(),
                additional_identifiers: vec![],
                issuer_requirements: vec![],
                holder_requirements: vec![],
                initial_instrument_issuers: vec![cid("issuer")],
                credential_valid_from_micros: from,
                credential_valid_until_micros: until,
                use_block_list: None,
            }),
            Box::new(OnboardInstrumentIssuers {
                instrument_configuration_cid: "00icc".to_string(),
                instrument_issuers: vec![cid("issuer")],
                credential_valid_from_micros: from,
                credential_valid_until_micros: until,
            }),
        ]
    };
    let accepted = [
        (None, None),
        (Some(now - 1), None),
        (None, Some(now + 1)),
        (Some(now - 1), Some(now + 1)),
        // A window that opens after now is the operator's call.
        (Some(now + 10), Some(now + 20)),
    ];
    let refused = [
        // Already ended: execution would mint expired credentials.
        (None, Some(now)),
        (Some(now - 20), Some(now - 10)),
        // Empty or inverted window: the ensure clause refuses it.
        (Some(now + 10), Some(now + 10)),
        (Some(now + 20), Some(now + 10)),
        // Not a timestamp after the epoch.
        (Some(0), None),
    ];
    for (from, until) in accepted {
        for proposal in with_window(from, until) {
            assert!(
                proposal.validate(&ctx(&gov, now)).is_ok(),
                "{from:?}..{until:?} should be accepted"
            );
        }
    }
    for (from, until) in refused {
        for proposal in with_window(from, until) {
            assert!(
                proposal.validate(&ctx(&gov, now)).is_err(),
                "{from:?}..{until:?} should be refused"
            );
        }
    }
}

#[test]
fn onboard_instrument_issuers_rejects_empty_issuer_list() {
    // Mirrors the template's `ensure not (null instrumentIssuers)` so the
    // rejection surfaces as a 400 before the ledger sees the proposal.
    let gov = cid("gov");
    let mk = |issuers: Vec<CantonId>| OnboardInstrumentIssuers {
        instrument_configuration_cid: "icc".to_string(),
        instrument_issuers: issuers,
        credential_valid_from_micros: None,
        credential_valid_until_micros: None,
    };
    assert!(mk(Vec::new()).validate(&ctx(&gov, 0)).is_err());
    assert!(mk(vec![cid("issuer")]).validate(&ctx(&gov, 0)).is_ok());
}

#[test]
fn onboard_instrument_issuers_rejects_duplicate_issuers() {
    // Mirrors the template's `ensure unique instrumentIssuers`: a
    // duplicated issuer would mint two credentials sharing one id, so
    // the rejection surfaces as a 400 before the ledger sees it.
    let gov = cid("gov");
    let issuer_a = cid("issuer-a");
    let issuer_b = cid("issuer-b");
    let mk = |issuers: Vec<CantonId>| OnboardInstrumentIssuers {
        instrument_configuration_cid: "icc".to_string(),
        instrument_issuers: issuers,
        credential_valid_from_micros: None,
        credential_valid_until_micros: None,
    };
    assert!(
        mk(vec![issuer_a.clone(), issuer_a.clone()])
            .validate(&ctx(&gov, 0))
            .is_err()
    );
    assert!(mk(vec![issuer_a, issuer_b]).validate(&ctx(&gov, 0)).is_ok());
}

#[test]
fn offboard_instrument_issuers_validates_rows() {
    // Mirrors the template's four ensure guards.
    let gov = cid("gov");
    let issuer_a = cid("issuer-a");
    let issuer_b = cid("issuer-b");
    let row = |issuer: CantonId, cids: Vec<&str>| InstrumentIssuerCredentials {
        instrument_issuer: issuer,
        credential_cids: cids.into_iter().map(str::to_string).collect(),
    };
    let mk = |rows: Vec<InstrumentIssuerCredentials>| OffboardInstrumentIssuers {
        instrument_issuers: rows,
    };

    // No rows: revokes nothing.
    assert!(mk(vec![]).validate(&ctx(&gov, 0)).is_err());
    // A row with no cids: revokes nothing.
    assert!(
        mk(vec![row(issuer_a.clone(), vec![])])
            .validate(&ctx(&gov, 0))
            .is_err()
    );
    // The same party in two rows.
    assert!(
        mk(vec![
            row(issuer_a.clone(), vec!["cred-1"]),
            row(issuer_a.clone(), vec!["cred-2"]),
        ])
        .validate(&ctx(&gov, 0))
        .is_err()
    );
    // The same cid in two rows.
    assert!(
        mk(vec![
            row(issuer_a.clone(), vec!["cred-1"]),
            row(issuer_b.clone(), vec!["cred-1"]),
        ])
        .validate(&ctx(&gov, 0))
        .is_err()
    );
    // The same cid twice inside one row.
    assert!(
        mk(vec![row(issuer_a.clone(), vec!["cred-1", "cred-1"])])
            .validate(&ctx(&gov, 0))
            .is_err()
    );
    // Two issuers, distinct cids.
    assert!(
        mk(vec![
            row(issuer_a, vec!["cred-1", "cred-2"]),
            row(issuer_b, vec!["cred-3"]),
        ])
        .validate(&ctx(&gov, 0))
        .is_ok()
    );
}

#[test]
fn provision_instrument_rejects_duplicate_initial_issuers() {
    // Mirrors the template's `ensure unique initialInstrumentIssuers`.
    // An empty list stays legal: issuers can be onboarded later.
    let gov = cid("gov");
    let issuer_a = cid("issuer-a");
    let issuer_b = cid("issuer-b");
    let mk = |issuers: Vec<CantonId>| ProvisionInstrument {
        registrar_service_cid: "rsc".to_string(),
        instrument_id_text: "uuid-1".to_string(),
        additional_identifiers: vec![],
        issuer_requirements: vec![],
        holder_requirements: vec![],
        initial_instrument_issuers: issuers,
        credential_valid_from_micros: None,
        credential_valid_until_micros: None,
        use_block_list: None,
    };
    assert!(
        mk(vec![issuer_a.clone(), issuer_a.clone()])
            .validate(&ctx(&gov, 0))
            .is_err()
    );
    assert!(mk(vec![issuer_a, issuer_b]).validate(&ctx(&gov, 0)).is_ok());
    assert!(mk(Vec::new()).validate(&ctx(&gov, 0)).is_ok());
}

#[test]
fn provision_instrument_rejects_claimless_self_issued_requirement() {
    // The same guard on the other template that carries it in Daml.
    let gov = cid("gov");
    let mk = |issuer: CantonId, claims: Vec<RequiredClaim>| ProvisionInstrument {
        registrar_service_cid: "rsc".to_string(),
        instrument_id_text: "uuid-1".to_string(),
        additional_identifiers: vec![],
        issuer_requirements: vec![PartyCredentialRequirement {
            issuer,
            required_claims: claims,
        }],
        holder_requirements: vec![],
        initial_instrument_issuers: vec![],
        credential_valid_from_micros: None,
        credential_valid_until_micros: None,
        use_block_list: None,
    };
    let claim = RequiredClaim {
        property: "role".to_string(),
        value: "instrument-issuer".to_string(),
    };
    assert!(mk(gov.clone(), vec![]).validate(&ctx(&gov, 0)).is_err());
    assert!(mk(gov.clone(), vec![claim]).validate(&ctx(&gov, 0)).is_ok());
    assert!(mk(cid("other"), vec![]).validate(&ctx(&gov, 0)).is_ok());
}

#[test]
fn update_instrument_configuration_rejects_claimless_self_issued_requirement() {
    // Mirrors the template's `ensure`, as on ProvisionInstrument.
    let gov = cid("gov");
    let mk = |issuer: CantonId, claims: Vec<RequiredClaim>| UpdateInstrumentConfiguration {
        registrar_service_cid: "rsc".to_string(),
        instrument_configuration_cid: "icc".to_string(),
        instrument_id_text: "uuid-1".to_string(),
        additional_identifiers: vec![],
        issuer_requirements: vec![PartyCredentialRequirement {
            issuer,
            required_claims: claims,
        }],
        holder_requirements: vec![],
        use_block_list: None,
    };
    let claim = RequiredClaim {
        property: "role".to_string(),
        value: "instrument-issuer".to_string(),
    };
    assert!(mk(gov.clone(), vec![]).validate(&ctx(&gov, 0)).is_err());
    assert!(mk(gov.clone(), vec![claim]).validate(&ctx(&gov, 0)).is_ok());
    assert!(mk(cid("other"), vec![]).validate(&ctx(&gov, 0)).is_ok());
}

/// Mirrors the templates' `blockListSupported` guard: not every mint,
/// burn and transfer path sends block list shards yet, so `Some(true)` is
/// refused with a 400 that names #566.
#[test]
fn configuration_proposals_refuse_use_block_list_true() {
    let gov = cid("gov");
    let with_flag = |use_block_list: Option<bool>| -> Vec<(&str, Box<dyn Validate>)> {
        vec![
            (
                "setup_utility",
                Box::new(SetupUtility {
                    provider_service_cid: "psc".to_string(),
                    operator: cid("op"),
                    instrument_id_text: "uuid-1".to_string(),
                    additional_identifiers: vec![],
                    create_transfer_rule: true,
                    create_allocation_factory: true,
                    use_block_list,
                }),
            ),
            (
                "provision_instrument",
                Box::new(ProvisionInstrument {
                    registrar_service_cid: "rsc".to_string(),
                    instrument_id_text: "uuid-1".to_string(),
                    additional_identifiers: vec![],
                    issuer_requirements: vec![],
                    holder_requirements: vec![],
                    initial_instrument_issuers: vec![],
                    credential_valid_from_micros: None,
                    credential_valid_until_micros: None,
                    use_block_list,
                }),
            ),
            (
                "update_instrument_configuration",
                Box::new(UpdateInstrumentConfiguration {
                    registrar_service_cid: "rsc".to_string(),
                    instrument_configuration_cid: "icc".to_string(),
                    instrument_id_text: "uuid-1".to_string(),
                    additional_identifiers: vec![],
                    issuer_requirements: vec![],
                    holder_requirements: vec![],
                    use_block_list,
                }),
            ),
        ]
    };
    for flag in [None, Some(false)] {
        for (name, proposal) in with_flag(flag) {
            assert!(
                proposal.validate(&ctx(&gov, 0)).is_ok(),
                "{name} should accept use_block_list {flag:?}"
            );
        }
    }
    for (name, proposal) in with_flag(Some(true)) {
        match proposal.validate(&ctx(&gov, 0)) {
            Err(Error::Validation(message)) => assert!(
                message.contains("use_block_list") && message.contains("#566"),
                "{name}: unexpected message {message:?}"
            ),
            other => panic!("{name} should refuse use_block_list true, got {other:?}"),
        }
    }
}

#[test]
fn create_provider_configuration_rejects_claimless_self_issued_requirement() {
    // Mirrors the template's `selfIssuedRequirementsHaveClaims`. The
    // frontend prefills a new requirement row as the governance party
    // with no claims, so the default UI path trips this.
    let gov = cid("gov");
    let mk = |issuer: CantonId, claims: Vec<RequiredClaim>| CreateProviderConfiguration {
        provider_service_cid: "psc".to_string(),
        registrar_requirements: vec![PartyCredentialRequirement {
            issuer,
            required_claims: claims,
        }],
        holder_requirements: vec![],
    };
    let claim = RequiredClaim {
        property: "role".to_string(),
        value: "registrar".to_string(),
    };
    // Self-issued and claimless: rejected.
    assert!(mk(gov.clone(), vec![]).validate(&ctx(&gov, 0)).is_err());
    // Self-issued with a claim: accepted.
    assert!(mk(gov.clone(), vec![claim]).validate(&ctx(&gov, 0)).is_ok());
    // Issued by another party and claimless: accepted, matching the Daml.
    assert!(mk(cid("other"), vec![]).validate(&ctx(&gov, 0)).is_ok());
}

#[test]
fn mint_and_burn_reject_non_positive_amount() {
    let gov = cid("gov");
    let mint = |amount: &str| Mint {
        allocation_factory_cid: "afc".to_string(),
        instrument_id: InstrumentId {
            admin: "admin::ns".to_string(),
            id: "instr-1".to_string(),
        },
        instrument_configuration_cid: "icc".to_string(),
        recipient: cid("recv"),
        amount: amount.parse().expect("valid decimal"),
        description: "mint it".to_string(),
        block_list_shard_cids: vec![],
    };
    assert!(mint("1.5").validate(&ctx(&gov, 0)).is_ok());
    assert!(mint("0").validate(&ctx(&gov, 0)).is_err());
    assert!(mint("-1").validate(&ctx(&gov, 0)).is_err());

    let burn = |amount: &str| Burn {
        allocation_factory_cid: "afc".to_string(),
        instrument_id: InstrumentId {
            admin: "admin::ns".to_string(),
            id: "instr-1".to_string(),
        },
        instrument_configuration_cid: "icc".to_string(),
        holder: cid("holder"),
        amount: amount.parse().expect("valid decimal"),
        description: "burn it".to_string(),
        block_list_shard_cids: vec![],
    };
    assert!(burn("1.5").validate(&ctx(&gov, 0)).is_ok());
    assert!(burn("0").validate(&ctx(&gov, 0)).is_err());
    assert!(burn("-1").validate(&ctx(&gov, 0)).is_err());
}

#[test]
fn set_provider_app_reward_beneficiaries_validates_weights_only_when_some() {
    let gov = cid("gov");
    let mk = |beneficiaries: Option<Vec<AppRewardBeneficiary>>| SetProviderAppRewardBeneficiaries {
        instrument_configuration_cid: "icc".to_string(),
        provider_app_reward_beneficiaries: beneficiaries,
    };
    // None is fine — the check only fires when a value is supplied.
    assert!(mk(None).validate(&ctx(&gov, 0)).is_ok());
    assert!(
        mk(Some(vec![AppRewardBeneficiary {
            beneficiary: cid("b1"),
            weight: "1.0".parse().expect("valid decimal"),
        }]))
        .validate(&ctx(&gov, 0))
        .is_ok()
    );
    assert!(
        mk(Some(vec![AppRewardBeneficiary {
            beneficiary: cid("b1"),
            weight: "0.5".parse().expect("valid decimal"),
        }]))
        .validate(&ctx(&gov, 0))
        .is_err()
    );
}
