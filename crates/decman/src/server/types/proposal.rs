//! The governance proposal types and their payloads.

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::canton_id::CantonId;

// ============================================================================
// Governance Types (Structured Actions)
// ============================================================================

/// Types of governance domain action proposals
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProposalType {
    /// Set up Canton Coin TransferPreapproval
    SetupCcPreapproval(decman_lib::catalog::proposals::custody::SetupCcPreapproval),
    /// Set up utility token TransferPreapproval
    SetupTokenPreapproval(decman_lib::catalog::proposals::custody::SetupTokenPreapproval),
    /// Transfer tokens via a TransferFactory
    Transfer(decman_lib::catalog::proposals::custody::Transfer),
    /// Accept an incoming token transfer
    AcceptTransfer(decman_lib::catalog::proposals::custody::AcceptTransfer),
    /// Generic text-based vote (no on-chain effect beyond recording the result)
    GenericVote(decman_lib::catalog::proposals::core::GenericVote),
    /// Provision a Utility-Registry `ProviderService` with
    /// `operator = proposer` and `provider = governanceParty`. Produces the
    /// ProviderService cid consumed by `SetupUtility`.
    ProvisionProviderService(decman_lib::catalog::proposals::utility::ProvisionProviderService),
    /// Run the full Utility-Registry onboarding in one vote. Flags control
    /// whether a `TransferRule` / `AllocationFactory` are created during the
    /// `RegistrarServiceRequest` accept.
    SetupUtility(decman_lib::catalog::proposals::utility::SetupUtility),
    /// Create a `ProviderServiceRequest` for a given `operator` and `provider`.
    CreateProviderServiceRequest(
        decman_lib::catalog::proposals::utility::CreateProviderServiceRequest,
    ),
    /// Create a `UserServiceRequest` for a given `operator` and `user`.
    CreateUserServiceRequest(decman_lib::catalog::proposals::utility::CreateUserServiceRequest),
    /// Set the provider-app reward beneficiaries on an `InstrumentConfiguration`.
    /// `providerAppRewardBeneficiaries = None` clears the current setting.
    SetProviderAppRewardBeneficiaries(
        decman_lib::catalog::proposals::utility::SetProviderAppRewardBeneficiaries,
    ),
    /// Create (or replace) the decparty's on-ledger CouponReassignmentDelegation.
    /// `prior_delegation` is the cid of the delegation being replaced (None for the first).
    SetupCouponReassignmentDelegation(
        decman_lib::catalog::proposals::rewards::SetupCouponReassignmentDelegation,
    ),
    /// Revoke (archive) the decparty's CouponReassignmentDelegation.
    RevokeCouponReassignmentDelegation(
        decman_lib::catalog::proposals::rewards::RevokeCouponReassignmentDelegation,
    ),
    /// Toggle result-contract emission on a `RegistrarService`.
    SetEnableResultContracts(decman_lib::catalog::proposals::utility::SetEnableResultContracts),
    /// Create a `TransferRule` on the governance party's `RegistrarService`.
    CreateTransferRule(decman_lib::catalog::proposals::utility::CreateTransferRule),
    /// Authorize the `operator` to create batched activity markers on behalf
    /// of the governance party via a `DelegatedBatchedMarkersProxy`.
    CreateDelegatedBatchedMarkersProxy(
        decman_lib::catalog::proposals::utility::CreateDelegatedBatchedMarkersProxy,
    ),
    /// Self-grant a `FeaturedAppRight` to the governance party on DevNet by
    /// exercising `AmuletRules_DevNet_FeatureApp`.
    RequestDevNetFeaturedAppRight(
        decman_lib::catalog::proposals::utility::RequestDevNetFeaturedAppRight,
    ),
    /// Delegate minting of the governance party's CIP-104 reward coupons to a
    /// validator node's `delegate` party via a `MintingDelegationProposal`.
    /// The delegation beneficiary is always the governance party; the delegate
    /// accepts the proposal out-of-band via the wallet API.
    SetupMintingDelegation(decman_lib::catalog::proposals::rewards::SetupMintingDelegation),
    /// Accept a validator-created `ExternalPartySetupProposal` on behalf of the
    /// governance party, creating its `ValidatorRight` + `TransferPreapproval`.
    /// This is the missing prerequisite that makes the validator's built-in
    /// `MintingDelegationCollectRewardsTrigger` start collecting the party's
    /// CIP-104 reward coupons via the established `MintingDelegation`.
    AcceptExternalPartySetup(decman_lib::catalog::proposals::rewards::AcceptExternalPartySetup),
    /// Offer a mint of `amount` tokens to `recipient` via
    /// `AllocationFactory_OfferMint`. The resulting `MintOffer` is accepted
    /// later by the recipient, outside this plugin.
    Mint(decman_lib::catalog::proposals::utility::Mint),
    /// Offer a free credential to a holder via the governance party's
    /// `UserService`. Wraps `UserService_OfferFreeCredential` from the
    /// Utility Credential App.
    OfferFreeCredential(decman_lib::catalog::proposals::credential::OfferFreeCredential),
    /// Offer a paid credential to a holder via the governance party's
    /// `UserService`. Wraps `UserService_OfferPaidCredential`.
    OfferPaidCredential(decman_lib::catalog::proposals::credential::OfferPaidCredential),
    /// Accept a free credential offered to the governance party. Wraps
    /// `UserService_AcceptFreeCredentialOffer`.
    AcceptFreeCredential(decman_lib::catalog::proposals::credential::AcceptFreeCredential),
    /// Accept a paid credential offered to the governance party. Wraps
    /// `UserService_AcceptPaidCredentialOffer`, which locks the governance
    /// party's deposit Amulets.
    AcceptPaidCredential(decman_lib::catalog::proposals::credential::AcceptPaidCredential),
    /// Offer a burn of `amount` tokens held by `holder` via
    /// `AllocationFactory_OfferBurn`. Holdings are supplied by the holder at
    /// `BurnOffer_Accept` time, not here.
    Burn(decman_lib::catalog::proposals::utility::Burn),
    /// Accept a holder-initiated `MintRequest` via `MintRequest_Accept`. The
    /// `MintRequest` must already exist on-ledger (typically created by the
    /// holder by exercising `AllocationFactory_RequestMint`).
    AcceptMintRequest(decman_lib::catalog::proposals::utility::AcceptMintRequest),
    /// Accept a holder-initiated `BurnRequest` via `BurnRequest_Accept`. The
    /// `BurnRequest` must already exist on-ledger (typically created by the
    /// holder by exercising `AllocationFactory_RequestBurn`).
    AcceptBurnRequest(decman_lib::catalog::proposals::utility::AcceptBurnRequest),
    /// Create the provider decparty's `ProviderConfiguration` with
    /// credential requirements for registrars and holders. Executed once by
    /// the provider decparty at platform setup.
    CreateProviderConfiguration(
        decman_lib::catalog::proposals::utility::CreateProviderConfiguration,
    ),
    /// Create a `RegistrarServiceRequest` asking `provider` for registrar
    /// service, with the governance party as the registrar. The provider
    /// accepts later via `OnboardRegistrar` on its own decparty.
    CreateRegistrarServiceRequest(
        decman_lib::catalog::proposals::utility::CreateRegistrarServiceRequest,
    ),
    /// Accept a `RegistrarServiceRequest` on the provider decparty: mint the
    /// registrar credentials the governance party can self-issue against the
    /// `ProviderConfiguration`'s registrar requirements, then accept the
    /// request in the same vote.
    OnboardRegistrar(decman_lib::catalog::proposals::utility::OnboardRegistrar),
    /// Create an `InstrumentConfiguration` on the registrar decparty and
    /// credential the initial instrument issuers against its issuer
    /// requirements. Executed once per instrument.
    ProvisionInstrument(decman_lib::catalog::proposals::utility::ProvisionInstrument),
    /// Replace a live `InstrumentConfiguration` on the registrar decparty
    /// with a full payload, then set its provider app reward beneficiaries
    /// again.
    UpdateInstrumentConfiguration(
        decman_lib::catalog::proposals::utility::UpdateInstrumentConfiguration,
    ),
    /// Credential new instrument issuers against an existing
    /// `InstrumentConfiguration`'s issuer requirements.
    OnboardInstrumentIssuers(decman_lib::catalog::proposals::utility::OnboardInstrumentIssuers),
    /// Revoke the credentials the governance party issued for instrument
    /// issuers, removing their issuing privileges. Each row names one issuer
    /// and lists that issuer's credentials.
    OffboardInstrumentIssuers(decman_lib::catalog::proposals::utility::OffboardInstrumentIssuers),
}

impl ProposalType {
    /// Validate the proposal's fields against the governance party the
    /// proposal targets. Mirrors `ActionType::validate` — catches bad input
    /// before it reaches Canton's Daml checks so a 400 surfaces a precise
    /// reason rather than a generic submission error.
    ///
    /// **Propose-path only.** The single production caller is
    /// `handlers::governance::propose_action`, and one arm
    /// (`decman_lib::framework::validate::validate_future_micros`) reads the
    /// clock. Re-using this to
    /// re-validate an already-stored proposal would reject it for nothing but
    /// having aged, so a new call site needs to split the time-dependent arms
    /// out first.
    pub fn validate(&self, governance_party: &CantonId) -> Result<(), String> {
        let ctx = decman_lib::framework::ValidationCtx {
            governance_party,
            now_micros: Utc::now().timestamp_micros(),
        };
        let payload: &dyn decman_lib::framework::Validate = match self {
            Self::SetupCcPreapproval(p) => p,
            Self::SetupTokenPreapproval(p) => p,
            Self::Transfer(p) => p,
            Self::AcceptTransfer(p) => p,
            Self::GenericVote(p) => p,
            Self::ProvisionProviderService(p) => p,
            Self::SetupUtility(p) => p,
            Self::CreateProviderServiceRequest(p) => p,
            Self::CreateUserServiceRequest(p) => p,
            Self::SetProviderAppRewardBeneficiaries(p) => p,
            Self::SetupCouponReassignmentDelegation(p) => p,
            Self::RevokeCouponReassignmentDelegation(p) => p,
            Self::SetEnableResultContracts(p) => p,
            Self::CreateTransferRule(p) => p,
            Self::CreateDelegatedBatchedMarkersProxy(p) => p,
            Self::RequestDevNetFeaturedAppRight(p) => p,
            Self::SetupMintingDelegation(p) => p,
            Self::AcceptExternalPartySetup(p) => p,
            Self::Mint(p) => p,
            Self::OfferFreeCredential(p) => p,
            Self::OfferPaidCredential(p) => p,
            Self::AcceptFreeCredential(p) => p,
            Self::AcceptPaidCredential(p) => p,
            Self::Burn(p) => p,
            Self::AcceptMintRequest(p) => p,
            Self::AcceptBurnRequest(p) => p,
            Self::CreateProviderConfiguration(p) => p,
            Self::CreateRegistrarServiceRequest(p) => p,
            Self::OnboardRegistrar(p) => p,
            Self::ProvisionInstrument(p) => p,
            Self::UpdateInstrumentConfiguration(p) => p,
            Self::OnboardInstrumentIssuers(p) => p,
            Self::OffboardInstrumentIssuers(p) => p,
        };
        payload.validate(&ctx).map_err(|e| e.to_string())
    }

    /// The generic propose payload — `None` for the two transfer variants,
    /// which need runtime context (the registry choice context, the validity
    /// window, and the on-chain sender party) and so go through their
    /// wrapper structs (`TransferWithContext` /
    /// `AcceptTransferWithContext`) rather than the payload itself.
    pub fn grpc_payload(&self) -> Option<&dyn decman_lib::framework::GrpcPayload> {
        match self {
            Self::Transfer(_) | Self::AcceptTransfer(_) => None,
            Self::SetupCcPreapproval(p) => Some(p),
            Self::SetupTokenPreapproval(p) => Some(p),
            Self::GenericVote(p) => Some(p),
            Self::ProvisionProviderService(p) => Some(p),
            Self::SetupUtility(p) => Some(p),
            Self::CreateProviderServiceRequest(p) => Some(p),
            Self::CreateUserServiceRequest(p) => Some(p),
            Self::SetProviderAppRewardBeneficiaries(p) => Some(p),
            Self::SetupCouponReassignmentDelegation(p) => Some(p),
            Self::RevokeCouponReassignmentDelegation(p) => Some(p),
            Self::SetEnableResultContracts(p) => Some(p),
            Self::CreateTransferRule(p) => Some(p),
            Self::CreateDelegatedBatchedMarkersProxy(p) => Some(p),
            Self::RequestDevNetFeaturedAppRight(p) => Some(p),
            Self::SetupMintingDelegation(p) => Some(p),
            Self::AcceptExternalPartySetup(p) => Some(p),
            Self::Mint(p) => Some(p),
            Self::OfferFreeCredential(p) => Some(p),
            Self::OfferPaidCredential(p) => Some(p),
            Self::AcceptFreeCredential(p) => Some(p),
            Self::AcceptPaidCredential(p) => Some(p),
            Self::Burn(p) => Some(p),
            Self::AcceptMintRequest(p) => Some(p),
            Self::AcceptBurnRequest(p) => Some(p),
            Self::CreateProviderConfiguration(p) => Some(p),
            Self::CreateRegistrarServiceRequest(p) => Some(p),
            Self::OnboardRegistrar(p) => Some(p),
            Self::ProvisionInstrument(p) => Some(p),
            Self::UpdateInstrumentConfiguration(p) => Some(p),
            Self::OnboardInstrumentIssuers(p) => Some(p),
            Self::OffboardInstrumentIssuers(p) => Some(p),
        }
    }

    /// The payload of a variant whose template mints governance credentials.
    /// `propose_action` submits these through `WithCredentialIdNonce` with
    /// a fresh nonce, so the request carries no nonce field.
    pub fn credential_minting_payload(
        &self,
    ) -> Option<&dyn decman_lib::catalog::proposals::utility::MintsCredentials> {
        match self {
            Self::OnboardRegistrar(p) => Some(p),
            Self::ProvisionInstrument(p) => Some(p),
            Self::OnboardInstrumentIssuers(p) => Some(p),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use common::api::InstrumentId;
    use decman_lib::catalog::{
        proposals::{
            core::GenericVote,
            credential::{
                AcceptFreeCredential, AcceptPaidCredential, OfferFreeCredential,
                OfferPaidCredential,
            },
            custody::{AcceptTransfer, SetupCcPreapproval, SetupTokenPreapproval, Transfer},
            rewards::{
                AcceptExternalPartySetup, RevokeCouponReassignmentDelegation,
                SetupCouponReassignmentDelegation, SetupMintingDelegation,
            },
            utility::{
                AcceptBurnRequest, AcceptMintRequest, Burn, CreateDelegatedBatchedMarkersProxy,
                CreateProviderConfiguration, CreateProviderServiceRequest,
                CreateRegistrarServiceRequest, CreateTransferRule, CreateUserServiceRequest, Mint,
                OffboardInstrumentIssuers, OnboardInstrumentIssuers, OnboardRegistrar,
                ProvisionInstrument, ProvisionProviderService, RequestDevNetFeaturedAppRight,
                SetEnableResultContracts, SetProviderAppRewardBeneficiaries, SetupUtility,
                UpdateInstrumentConfiguration,
            },
        },
        types::{AppTransferContextInput, BillingParams},
    };

    use super::{super::test_support::test_party, *};

    /// One minimal instance per `ProposalType` variant, in declaration order.
    /// Field values are placeholders — this only exercises which variants
    /// `grpc_payload` treats as `Some`/`None`, not the payload contents (see
    /// `serde_snapshots.rs` for the fully-populated wire-shape fixtures).
    fn one_of_each_proposal_type() -> Vec<ProposalType> {
        let instrument_id = || InstrumentId {
            admin: "admin-party".into(),
            id: "TOK".into(),
        };
        vec![
            ProposalType::SetupCcPreapproval(SetupCcPreapproval {
                provider: test_party("prov").unwrap(),
                expected_dso: test_party("dso").unwrap(),
            }),
            ProposalType::SetupTokenPreapproval(SetupTokenPreapproval {
                operator: test_party("op").unwrap(),
                instrument_admin: test_party("iadmin").unwrap(),
                instrument_allowances: vec![],
            }),
            ProposalType::Transfer(Transfer {
                transfer_factory_cid: "00tf".into(),
                expected_admin: test_party("iadmin").unwrap(),
                receiver: test_party("recv").unwrap(),
                amount: "1".parse().unwrap(),
                instrument_id: instrument_id(),
                input_holding_cids: vec![],
                validity_window_hours: None,
            }),
            ProposalType::AcceptTransfer(AcceptTransfer {
                transfer_instruction_cid: "00ti".into(),
            }),
            ProposalType::GenericVote(GenericVote {
                description: "a vote".into(),
            }),
            ProposalType::ProvisionProviderService(ProvisionProviderService {}),
            ProposalType::SetupUtility(SetupUtility {
                provider_service_cid: "00psc".into(),
                operator: test_party("op").unwrap(),
                instrument_id_text: "uuid-1".into(),
                additional_identifiers: vec![],
                create_transfer_rule: true,
                create_allocation_factory: true,
                use_block_list: None,
            }),
            ProposalType::CreateProviderServiceRequest(CreateProviderServiceRequest {
                operator: test_party("op").unwrap(),
                provider: test_party("prov").unwrap(),
            }),
            ProposalType::CreateUserServiceRequest(CreateUserServiceRequest {
                operator: test_party("op").unwrap(),
                user: test_party("user").unwrap(),
            }),
            ProposalType::SetProviderAppRewardBeneficiaries(SetProviderAppRewardBeneficiaries {
                instrument_configuration_cid: "00icc".into(),
                provider_app_reward_beneficiaries: None,
            }),
            ProposalType::SetupCouponReassignmentDelegation(SetupCouponReassignmentDelegation {
                dso: test_party("dso").unwrap(),
                assigners: vec![],
                new_beneficiaries: vec![],
                prior_delegation: None,
            }),
            ProposalType::RevokeCouponReassignmentDelegation(RevokeCouponReassignmentDelegation {
                delegation: "00deleg".into(),
            }),
            ProposalType::SetEnableResultContracts(SetEnableResultContracts {
                registrar_service_cid: "00rsc".into(),
                enable_result_contracts: None,
            }),
            ProposalType::CreateTransferRule(CreateTransferRule {
                registrar_service_cid: "00rsc".into(),
            }),
            ProposalType::CreateDelegatedBatchedMarkersProxy(CreateDelegatedBatchedMarkersProxy {
                operator: test_party("op").unwrap(),
            }),
            ProposalType::RequestDevNetFeaturedAppRight(RequestDevNetFeaturedAppRight {
                amulet_rules_cid: "00amulet".into(),
            }),
            ProposalType::SetupMintingDelegation(SetupMintingDelegation {
                delegate: test_party("delegate").unwrap(),
                dso: test_party("dso").unwrap(),
                expires_at_micros: 4_000_000_000_000_000,
                amulet_merge_limit: 10,
                description: "delegate minting".into(),
            }),
            ProposalType::AcceptExternalPartySetup(AcceptExternalPartySetup {
                proposal_cid: "00eps".into(),
            }),
            ProposalType::Mint(Mint {
                allocation_factory_cid: "00alloc".into(),
                instrument_id: instrument_id(),
                instrument_configuration_cid: "00icc".into(),
                recipient: test_party("recv").unwrap(),
                amount: "5".parse().unwrap(),
                description: "mint".into(),
                block_list_shard_cids: vec![],
            }),
            ProposalType::OfferFreeCredential(OfferFreeCredential {
                user_service_cid: "00usc".into(),
                holder: test_party("holder").unwrap(),
                id: "cred-1".into(),
                description: "free cred".into(),
                claims: vec![],
            }),
            ProposalType::OfferPaidCredential(OfferPaidCredential {
                user_service_cid: "00usc".into(),
                holder: test_party("holder").unwrap(),
                id: "cred-2".into(),
                description: "paid cred".into(),
                claims: vec![],
                billing_params: BillingParams {
                    fee_per_day_usd: "1.5".parse().unwrap(),
                    billing_period_minutes: 60,
                    deposit_target_amount_usd: "30".parse().unwrap(),
                    holder_activity_weight: None,
                },
                deposit_initial_amount_usd: None,
            }),
            ProposalType::AcceptFreeCredential(AcceptFreeCredential {
                user_service_cid: "00usc".into(),
                credential_offer_cid: "00offer".into(),
            }),
            ProposalType::AcceptPaidCredential(AcceptPaidCredential {
                user_service_cid: "00usc".into(),
                credential_offer_cid: "00offer".into(),
                deposit_amulet_cids: vec!["00amulet".into()],
                app_transfer_context: AppTransferContextInput {
                    amulet_rules_cid: "00rules".into(),
                    open_mining_round_cid: "00round".into(),
                    featured_app_right_cid: None,
                },
                deposit_amount_usd: None,
            }),
            ProposalType::Burn(Burn {
                allocation_factory_cid: "00alloc".into(),
                instrument_id: instrument_id(),
                instrument_configuration_cid: "00icc".into(),
                holder: test_party("holder").unwrap(),
                amount: "3".parse().unwrap(),
                description: "burn".into(),
                block_list_shard_cids: vec![],
            }),
            ProposalType::AcceptMintRequest(AcceptMintRequest {
                mint_request_cid: "00mr".into(),
                instrument_configuration_cid: "00icc".into(),
                issuer_credential_cids: vec![],
                description: "accept mint".into(),
                block_list_shard_cids: vec![],
            }),
            ProposalType::AcceptBurnRequest(AcceptBurnRequest {
                burn_request_cid: "00br".into(),
                instrument_configuration_cid: "00icc".into(),
                issuer_credential_cids: vec![],
                description: "accept burn".into(),
                block_list_shard_cids: vec![],
            }),
            ProposalType::CreateProviderConfiguration(CreateProviderConfiguration {
                provider_service_cid: "00psc".into(),
                registrar_requirements: vec![],
                holder_requirements: vec![],
            }),
            ProposalType::CreateRegistrarServiceRequest(CreateRegistrarServiceRequest {
                operator: test_party("op").unwrap(),
                provider: test_party("prov").unwrap(),
                create_transfer_rule: false,
                create_allocation_factory: true,
            }),
            ProposalType::OnboardRegistrar(OnboardRegistrar {
                provider_service_cid: "00psc".into(),
                registrar_service_request_cid: "00rsr".into(),
                provider_configuration_cid: "00pcc".into(),
            }),
            ProposalType::ProvisionInstrument(ProvisionInstrument {
                registrar_service_cid: "00rsc".into(),
                instrument_id_text: "uuid-2".into(),
                additional_identifiers: vec![],
                issuer_requirements: vec![],
                holder_requirements: vec![],
                initial_instrument_issuers: vec![],
                credential_valid_from_micros: None,
                credential_valid_until_micros: None,
                use_block_list: None,
            }),
            ProposalType::UpdateInstrumentConfiguration(UpdateInstrumentConfiguration {
                registrar_service_cid: "00rsc".into(),
                instrument_configuration_cid: "00icc".into(),
                instrument_id_text: "uuid-2".into(),
                additional_identifiers: vec![],
                issuer_requirements: vec![],
                holder_requirements: vec![],
                use_block_list: None,
            }),
            ProposalType::OnboardInstrumentIssuers(OnboardInstrumentIssuers {
                instrument_configuration_cid: "00icc".into(),
                instrument_issuers: vec![],
                credential_valid_from_micros: None,
                credential_valid_until_micros: None,
            }),
            ProposalType::OffboardInstrumentIssuers(OffboardInstrumentIssuers {
                instrument_issuers: vec![],
            }),
        ]
    }

    /// Guards the split the payload-projection macro used to generate: every
    /// `ProposalType` variant carries a `GrpcPayload` except `Transfer` and
    /// `AcceptTransfer`, which need runtime context and go through wrapper
    /// structs instead (see `grpc_payload`'s doc comment). A variant landing
    /// in the wrong match arm — Transfer wrongly returning `Some`, or a
    /// plain variant wrongly returning `None` — fails this test instead of
    /// surfacing as a silent gap deep in `propose_action`.
    #[test]
    fn grpc_payload_is_none_only_for_transfer_and_accept_transfer() {
        for proposal in one_of_each_proposal_type() {
            let is_transfer_variant = matches!(
                proposal,
                ProposalType::Transfer(_) | ProposalType::AcceptTransfer(_)
            );
            assert_eq!(
                proposal.grpc_payload().is_none(),
                is_transfer_variant,
                "{proposal:?}: grpc_payload() Some/None must match transfer-variant status"
            );
        }
    }

    /// Exactly the three variants whose templates mint governance
    /// credentials submit with a credential id nonce. A minting variant
    /// left out would mint ids that can collide again (#345).
    #[test]
    fn credential_minting_payload_is_some_only_for_minting_variants() {
        for proposal in one_of_each_proposal_type() {
            let mints = matches!(
                proposal,
                ProposalType::OnboardRegistrar(_)
                    | ProposalType::ProvisionInstrument(_)
                    | ProposalType::OnboardInstrumentIssuers(_)
            );
            assert_eq!(
                proposal.credential_minting_payload().is_some(),
                mints,
                "{proposal:?}: credential_minting_payload() Some/None must match minting status"
            );
        }
    }
}
