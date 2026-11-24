//! Ledger submissions for each governance action.

use std::collections::HashSet;

use anyhow::Context;
use base64::Engine;
use canton_proto_rs::com::daml::ledger::api::v2::{
    CreatedEvent, DisclosedContract, SubmitAndWaitRequest,
    command_service_client::CommandServiceClient, value,
};
use decman_lib::{
    catalog::{
        commands::{
            build_cancel_domain_confirmation, build_cancel_proposal,
            build_cancel_self_confirmation, build_confirm_proposal, build_confirm_self_action,
            build_execute_proposal, build_execute_self_action, build_expire_domain_confirmation,
            build_expire_self_confirmation,
        },
        proposals::{credential::AcceptPaidCredential, utility::RequestDevNetFeaturedAppRight},
        templates::{
            domain_confirmation_template, governable_action_interface, governance_rules_template,
            self_confirmation_template,
        },
    },
    framework::record::record_field,
};

use super::disclosures::{PaidAcceptPins, amulet_rules_disclosure, paid_accept_disclosures};
use crate::{
    canton_id::CantonId,
    config::{NodeConfig, PackageConfig},
    error::Result,
    server::{
        handlers::token_standard::fetch_amulet_rules,
        queries::resolve_contract_package_ref,
        transfer_context::{
            fetch_proposal_created_event, has_template, maybe_fetch_for_proposal_event,
            to_proto_disclosed_contracts,
        },
        types::{
            CancelConfirmationRequest, CancelProposalRequest, ConfirmActionRequest,
            ExecuteActionRequest, ExpireConfirmationRequest, GovernanceType,
        },
    },
    utils,
};

// ============================================================================
// Ledger Command Execution
// ============================================================================

/// Execute ConfirmAction choice on the GovernanceRules contract with structured action
pub(super) async fn execute_confirm_action(
    config: &NodeConfig,
    request: &ConfirmActionRequest,
    token: &str,
    member_party_id: &CantonId,
    packages: &PackageConfig,
) -> Result {
    let command_id = uuid::Uuid::new_v4().to_string();
    let commands = match request.governance_type {
        GovernanceType::CoreSelf => {
            let mut rules = governance_rules_template(packages)?;
            // The rules contract may be an out-of-date fallback living under an
            // older governance-core package — exercise it under its actual
            // package ref.
            rules.package_ref = resolve_contract_package_ref(
                config,
                &request.party_id,
                Some(token.to_string()),
                &request.rules_contract_id,
                &rules.package_ref,
            )
            .await;
            build_confirm_self_action(
                &rules,
                &request.rules_contract_id,
                member_party_id,
                &request.party_id,
                &request.action,
                command_id,
            )?
        }
        GovernanceType::CoreDomain => {
            let mut rules = governance_rules_template(packages)?;
            let proposal_cid = request
                .proposal_cid
                .as_deref()
                .context("proposal_cid required for core_domain confirm")?;
            rules.package_ref = resolve_contract_package_ref(
                config,
                &request.party_id,
                Some(token.to_string()),
                &request.rules_contract_id,
                &rules.package_ref,
            )
            .await;
            build_confirm_proposal(
                &rules,
                &request.rules_contract_id,
                member_party_id,
                &request.party_id,
                proposal_cid,
                command_id,
            )
        }
    };

    let channel = config.ledger_channel().await?;

    let mut client =
        CommandServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let mut req = tonic::Request::new(SubmitAndWaitRequest {
        commands: Some(commands),
    });
    req.metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());

    client.submit_and_wait(req).await?;

    Ok(())
}

/// Execute ExecuteConfirmedAction choice on governance rules contract
pub(super) async fn execute_confirmed_action(
    config: &NodeConfig,
    http_client: &reqwest::Client,
    request: &ExecuteActionRequest,
    token: &str,
    member_party_id: &CantonId,
    packages: &PackageConfig,
) -> Result {
    // The executor's submission must carry the contracts the proposal's
    // execute step reads but no member can see. `AcceptTransferProposal` needs
    // the registry-supplied transfer rule and its dependencies;
    // `RequestDevNetFeaturedAppRight` needs the DSO's `AmuletRules`;
    // `AcceptPaidCredential` needs `AmuletRules`, its open mining round and any
    // featured app right it names. All key on the on-chain proposal's
    // template, not on `governance_type`, so a non-CoreDomain path carrying
    // such a proposal still gets its contracts.
    let mut registry_disclosed: Vec<DisclosedContract> = Vec::new();
    let mut proposal_event: Option<CreatedEvent> = None;
    if let Some(proposal_cid) = request.proposal_cid.as_deref() {
        match fetch_proposal_created_event(
            config,
            Some(token.to_string()),
            &request.party_id,
            proposal_cid,
        )
        .await
        {
            Ok(event) => proposal_event = event,
            // Don't hard-fail on ledger hiccups here; the submission below
            // surfaces a clear error if the proposal really is gone.
            Err(e) => tracing::warn!("Failed to look up proposal {proposal_cid}: {e:#}"),
        }
    }
    if let (Some(proposal_cid), Some(created)) =
        (request.proposal_cid.as_deref(), proposal_event.as_ref())
    {
        match maybe_fetch_for_proposal_event(
            config,
            Some(token.to_string()),
            &request.party_id,
            created,
        )
        .await
        {
            Ok(Some(ctx)) => match to_proto_disclosed_contracts(&ctx.disclosed_contracts) {
                Ok(dcs) => registry_disclosed = dcs,
                Err(e) => {
                    // A malformed blob from the registry shouldn't 500 the
                    // execute call — mirror the non-fatal handling below.
                    tracing::warn!(
                        "Failed to decode registry disclosed contracts for proposal {proposal_cid}: {e:#}"
                    );
                }
            },
            Ok(None) => {}
            Err(e) => {
                // Don't hard-fail on registry hiccups for non-transfer
                // proposals; for transfer proposals the Daml choice will
                // surface a clear `Missing context entry` error.
                tracing::warn!(
                    "Failed to fetch transfer choice context for proposal {proposal_cid}: {e:#}"
                );
            }
        }

        if has_template(
            created,
            RequestDevNetFeaturedAppRight::MODULE,
            RequestDevNetFeaturedAppRight::ENTITY,
        ) {
            let proposed_cid = created
                .create_arguments
                .as_ref()
                .and_then(|args| record_field(args, "amuletRulesCid"))
                .and_then(|v| match v {
                    value::Sum::ContractId(cid) => Some(cid.as_str()),
                    _ => None,
                });
            let current = fetch_amulet_rules(http_client, config)
                .await
                .context("Failed to fetch AmuletRules from the DSO API")?;
            registry_disclosed.push(amulet_rules_disclosure(proposed_cid, &current)?);
        }

        if has_template(
            created,
            AcceptPaidCredential::MODULE,
            AcceptPaidCredential::ENTITY,
        ) {
            let pins = PaidAcceptPins::of(created)?;
            let requested: HashSet<&str> = request
                .disclosed_contracts
                .iter()
                .map(|dc| dc.contract_id.as_str())
                .collect();
            registry_disclosed
                .extend(paid_accept_disclosures(http_client, config, &pins, &requested).await?);
        }
    }

    let mut disclosed_contracts: Vec<DisclosedContract> = request
        .disclosed_contracts
        .iter()
        .map(|dc| {
            Ok(DisclosedContract {
                template_id: None,
                contract_id: dc.contract_id.clone(),
                created_event_blob: base64::engine::general_purpose::STANDARD
                    .decode(&dc.blob)
                    .context("Invalid base64 in disclosed contract blob")?,
                synchronizer_id: String::new(),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    // De-dup by contract id — the FE may forward something the registry also
    // returned; keep the first occurrence (FE-supplied).
    let seen: HashSet<String> = disclosed_contracts
        .iter()
        .map(|d| d.contract_id.clone())
        .collect();
    disclosed_contracts.extend(
        registry_disclosed
            .into_iter()
            .filter(|d| !seen.contains(&d.contract_id)),
    );

    let command_id = uuid::Uuid::new_v4().to_string();
    let commands = match request.governance_type {
        GovernanceType::CoreSelf => {
            let mut rules = governance_rules_template(packages)?;
            // The rules contract may be an out-of-date fallback living under an
            // older governance-core package — exercise it under its actual
            // package ref.
            rules.package_ref = resolve_contract_package_ref(
                config,
                &request.party_id,
                Some(token.to_string()),
                &request.rules_contract_id,
                &rules.package_ref,
            )
            .await;
            build_execute_self_action(
                &rules,
                &request.rules_contract_id,
                member_party_id,
                &request.party_id,
                &request.action,
                &request.confirmation_cids,
                disclosed_contracts,
                command_id,
            )?
        }
        GovernanceType::CoreDomain => {
            let mut rules = governance_rules_template(packages)?;
            let proposal_cid = request
                .proposal_cid
                .as_deref()
                .context("proposal_cid required for core_domain execute")?;
            rules.package_ref = resolve_contract_package_ref(
                config,
                &request.party_id,
                Some(token.to_string()),
                &request.rules_contract_id,
                &rules.package_ref,
            )
            .await;
            build_execute_proposal(
                &rules,
                &request.rules_contract_id,
                member_party_id,
                &request.party_id,
                proposal_cid,
                &request.confirmation_cids,
                disclosed_contracts,
                command_id,
            )
        }
    };

    let channel = config.ledger_channel().await?;

    let mut client =
        CommandServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let mut req = tonic::Request::new(SubmitAndWaitRequest {
        commands: Some(commands),
    });
    req.metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());

    client.submit_and_wait(req).await?;

    Ok(())
}

/// Execute ExpireConfirmation choice on governance rules contract
pub(super) async fn execute_expire_confirmation(
    config: &NodeConfig,
    request: &ExpireConfirmationRequest,
    token: &str,
    member_party_id: &CantonId,
    packages: &PackageConfig,
) -> Result {
    // All three flows take the same argument shape ({ member,
    // staleConfirmationCid }); only the template and the choice differ.
    let command_id = uuid::Uuid::new_v4().to_string();
    let commands = match request.governance_type {
        GovernanceType::CoreSelf => {
            let mut rules = governance_rules_template(packages)?;
            // The rules contract may be an out-of-date fallback living under an
            // older governance-core package — exercise it under its actual
            // package ref.
            rules.package_ref = resolve_contract_package_ref(
                config,
                &request.party_id,
                Some(token.to_string()),
                &request.rules_contract_id,
                &rules.package_ref,
            )
            .await;
            build_expire_self_confirmation(
                &rules,
                &request.rules_contract_id,
                member_party_id,
                &request.party_id,
                &request.confirmation_cid,
                command_id,
            )
        }
        GovernanceType::CoreDomain => {
            // Same `GovernanceRules` template as CoreSelf but a different choice:
            // `GovernanceRules_ExpireConfirmation` operates on the
            // `GovernanceConfirmation` template (domain action confirmations)
            // rather than `GovernanceSelfConfirmation`.
            let mut rules = governance_rules_template(packages)?;
            rules.package_ref = resolve_contract_package_ref(
                config,
                &request.party_id,
                Some(token.to_string()),
                &request.rules_contract_id,
                &rules.package_ref,
            )
            .await;
            build_expire_domain_confirmation(
                &rules,
                &request.rules_contract_id,
                member_party_id,
                &request.party_id,
                &request.confirmation_cid,
                command_id,
            )
        }
    };

    let channel = config.ledger_channel().await?;

    let mut client =
        CommandServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let mut req = tonic::Request::new(SubmitAndWaitRequest {
        commands: Some(commands),
    });
    req.metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());

    client.submit_and_wait(req).await?;

    Ok(())
}

/// Execute Cancel choice directly on a confirmation contract
pub(super) async fn execute_cancel_confirmation(
    config: &NodeConfig,
    request: &CancelConfirmationRequest,
    token: &str,
    member_party_id: &CantonId,
    packages: &PackageConfig,
) -> Result {
    // Every `_Cancel` choice is controller=confirmer and takes no arguments;
    // only the confirmation template differs.
    let command_id = uuid::Uuid::new_v4().to_string();
    let commands = match request.governance_type {
        GovernanceType::CoreSelf => {
            let mut confirmation = self_confirmation_template(packages)?;
            // The confirmation contract is created by the rules contract's
            // choice, so it shares the rules contract's (possibly out-of-date)
            // package — exercise it under its actual package ref.
            confirmation.package_ref = resolve_contract_package_ref(
                config,
                &request.party_id,
                Some(token.to_string()),
                &request.confirmation_cid,
                &confirmation.package_ref,
            )
            .await;
            build_cancel_self_confirmation(
                &confirmation,
                &request.confirmation_cid,
                member_party_id,
                &request.party_id,
                command_id,
            )
        }
        GovernanceType::CoreDomain => {
            // Domain confirmations live in their own template
            // `GovernanceConfirmation` (module `Governance.Confirmation`).
            let mut confirmation = domain_confirmation_template(packages)?;
            confirmation.package_ref = resolve_contract_package_ref(
                config,
                &request.party_id,
                Some(token.to_string()),
                &request.confirmation_cid,
                &confirmation.package_ref,
            )
            .await;
            build_cancel_domain_confirmation(
                &confirmation,
                &request.confirmation_cid,
                member_party_id,
                &request.party_id,
                command_id,
            )
        }
    };

    let channel = config.ledger_channel().await?;

    let mut client =
        CommandServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let mut req = tonic::Request::new(SubmitAndWaitRequest {
        commands: Some(commands),
    });
    req.metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());

    client.submit_and_wait(req).await?;

    Ok(())
}

/// Exercise `GovernableAction_ProposerCancel` on a proposal, and archive the
/// caller's own confirmation alongside it.
///
/// The two commands share one submission so they succeed or fail together. A
/// confirmation that outlives its proposal is only clearable by its own
/// confirmer: `GovernanceConfirmation_Expire` refuses to run before the
/// confirmation's expiry time.
pub(super) async fn execute_cancel_proposal(
    config: &NodeConfig,
    request: &CancelProposalRequest,
    token: &str,
    member_party_id: &CantonId,
    packages: &PackageConfig,
) -> Result {
    // `GovernableAction_ProposerCancel` is declared on the interface rather
    // than on any one template, so the exercise carries the `GovernableAction`
    // interface id and never the id of the template that actually created the
    // contract. That also rules out `resolve_contract_package_ref` here: it
    // reports the package of the contract, which is the proposal's own
    // package, not the interface's.
    let interface = governable_action_interface(packages)?;

    let own_confirmation = match request.confirmation_cid.as_deref() {
        Some(cid) => {
            let mut confirmation = domain_confirmation_template(packages)?;
            // The confirmation is created by the rules contract's choice, so it
            // shares that contract's possibly out-of-date package.
            confirmation.package_ref = resolve_contract_package_ref(
                config,
                &request.party_id,
                Some(token.to_string()),
                cid,
                &confirmation.package_ref,
            )
            .await;
            Some(confirmation)
        }
        None => None,
    };

    let commands = build_cancel_proposal(
        &interface,
        &request.proposal_cid,
        request
            .confirmation_cid
            .as_deref()
            .zip(own_confirmation.as_ref()),
        member_party_id,
        &request.party_id,
        uuid::Uuid::new_v4().to_string(),
    );

    let channel = config.ledger_channel().await?;

    let mut client =
        CommandServiceClient::new(channel).max_decoding_message_size(utils::MAX_GRPC_MESSAGE_SIZE);

    let mut req = tonic::Request::new(SubmitAndWaitRequest {
        commands: Some(commands),
    });
    req.metadata_mut()
        .insert("authorization", format!("Bearer {token}").parse().unwrap());

    client.submit_and_wait(req).await?;

    Ok(())
}
