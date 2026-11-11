//! Utility-registry and token-standard read models.

use serde::{Deserialize, Serialize};

use super::{Claim, InstrumentIdentifier, PartyCredentialRequirement};
use crate::canton_id::CantonId;

/// Information about a ProviderService contract
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ProviderServiceInfo {
    pub contract_id: String,
    pub operator: CantonId,
    pub provider: CantonId,
}

/// Response for the provider services endpoint
#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ProviderServicesResponse {
    pub services: Vec<ProviderServiceInfo>,
}

/// Information about a UserService contract
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct UserServiceInfo {
    pub contract_id: String,
    pub operator: CantonId,
    pub user: CantonId,
}

/// Response for the user services endpoint
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct UserServicesResponse {
    pub services: Vec<UserServiceInfo>,
}

/// Information about an InstrumentConfiguration contract (one "token" the
/// governance party can mint/burn against). `instrument_admin` and
/// `instrument_id` are read off the contract's `defaultIdentifier` field and
/// match the `InstrumentId { admin, id }` shape required by Mint/Burn
/// proposals.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct InstrumentInfo {
    pub contract_id: String,
    pub instrument_admin: CantonId,
    pub instrument_id: String,
    /// The fields an `update_instrument_configuration` proposal replaces, so
    /// its form can start from the current values. `None` when the contract
    /// could not be read.
    #[serde(default)]
    pub configuration: Option<InstrumentConfigurationFields>,
}

/// The replaceable fields of an `InstrumentConfiguration`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct InstrumentConfigurationFields {
    pub additional_identifiers: Vec<InstrumentIdentifier>,
    pub issuer_requirements: Vec<PartyCredentialRequirement>,
    pub holder_requirements: Vec<PartyCredentialRequirement>,
    pub check_block_list: Option<bool>,
}

/// Response for the instruments endpoint
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct InstrumentsResponse {
    pub instruments: Vec<InstrumentInfo>,
}

/// Active `Splice.Api.Token.TransferInstructionV1:TransferFactory` contract
/// visible to the party. The frontend joins these to the party's holdings by
/// `expected_admin == holding.instrument_admin` to prefill the TransferFactory
/// CID and expected-admin fields on the Transfer Proposal form.
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TransferFactoryInfo {
    pub contract_id: String,
    pub expected_admin: CantonId,
}

/// Response for the transfer-factories endpoint.
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TransferFactoriesResponse {
    pub transfer_factories: Vec<TransferFactoryInfo>,
}

/// A `Utility.Credential.V0.Credential:Credential` contract visible to the
/// party. The accept mint/burn request forms list these so the issuer
/// credentials backing the accept no longer have to be pasted in by hand.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct CredentialInfo {
    pub contract_id: String,
    pub issuer: CantonId,
    pub holder: CantonId,
    /// The template's `id` field — the credential's identifier.
    pub credential_id: String,
    pub description: String,
    /// The credential's claims. Each claim's `subject` names the party the
    /// credential attests for.
    pub claims: Vec<Claim>,
}

/// Response for the credentials endpoint
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct CredentialsResponse {
    pub credentials: Vec<CredentialInfo>,
}

/// Information about a RegistrarService contract
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct RegistrarServiceInfo {
    pub contract_id: String,
    pub operator: CantonId,
    pub registrar: CantonId,
}

/// Response for the registrar services endpoint
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct RegistrarServicesResponse {
    pub services: Vec<RegistrarServiceInfo>,
}

/// A pending `RegistrarServiceRequest` contract visible to the party. The
/// OnboardRegistrar form lists these so the request backing the onboard can
/// be picked instead of pasted in by hand.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct RegistrarServiceRequestInfo {
    pub contract_id: String,
    pub operator: CantonId,
    pub provider: CantonId,
    pub registrar: CantonId,
    /// The request's `createTransferRule` flag. The SDK treats an absent
    /// flag like `false`, and this field mirrors that reading.
    pub create_transfer_rule: bool,
    /// The request's `createAllocationFactory` flag, read like
    /// `create_transfer_rule`.
    pub create_allocation_factory: bool,
}

/// Response for the registrar service requests endpoint
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct RegistrarServiceRequestsResponse {
    pub registrar_service_requests: Vec<RegistrarServiceRequestInfo>,
}

/// A `ProviderConfiguration` contract visible to the party. The
/// OnboardRegistrar form lists these so the configuration backing the
/// onboard can be picked instead of pasted in by hand.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ProviderConfigurationInfo {
    pub contract_id: String,
    pub operator: CantonId,
    pub provider: CantonId,
}

/// Response for the provider configurations endpoint
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ProviderConfigurationsResponse {
    pub provider_configurations: Vec<ProviderConfigurationInfo>,
}

/// A `StoredBlockListPartyShas` shard of a registrar's block list, visible
/// to the party. The accept mint and burn request forms list these, so the
/// shards for an instrument that checks a block list can be picked.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct BlockListShardInfo {
    pub contract_id: String,
    pub operator: CantonId,
    pub registrar: CantonId,
    /// Hex prefix of the party hashes the shard covers.
    pub prefix: String,
}

/// Response for the block list shards endpoint
#[derive(Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct BlockListShardsResponse {
    pub block_list_shards: Vec<BlockListShardInfo>,
}

/// A contract ID with its blob
#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ContractWithBlob {
    pub contract_id: String,
    pub blob: String,
    /// The contract's decoded fields, only when the query asked for
    /// `include_payload`: the create arguments for a template query, the
    /// queried interface's view for an interface query. Same JSON shape as the
    /// chain audit's `details`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "typegen", ts(type = "any"))]
    pub payload: Option<serde_json::Value>,
}

/// DSO network info (amulet rules + DSO party)
#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct NetworkInfo {
    pub dso_party_id: CantonId,
    pub amulet_rules_cid: String,
    pub amulet_rules_blob: String,
}

/// The newest open `OpenMiningRound`, as the scan API lists it. A paid
/// credential accept pins this round.
#[derive(Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct OpenMiningRoundInfo {
    pub contract_id: String,
    /// The round number.
    pub round: u64,
    /// When the round opened, as RFC 3339.
    pub opens_at: String,
    /// When the round is due to close, as RFC 3339.
    pub target_closes_at: String,
}

/// DA Utility operator info (operator party id)
#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct OperatorInfo {
    pub party_id: CantonId,
}

/// Count of active `TransferPreapproval` contracts a governance party already
/// has, split by direction. The UI uses this to warn the user that re-issuing
/// a `SetupCcPreapproval` / `SetupTokenPreapproval` proposal is pointless
/// (the on-chain choice would fail when executed).
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TransferPreapprovalsResponse {
    /// `Splice.Wallet.TransferPreapproval:TransferPreapproval` — Canton Coin
    pub cc: usize,
    /// `Utility.Registry.App.V0.Model.TransferPreapproval:TransferPreapproval`
    pub token: usize,
}

/// Response for the generic contract query endpoint
#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ContractQueryResponse {
    pub contracts: Vec<ContractWithBlob>,
}

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use super::*;
    use crate::api::fixtures::party;
    use crate::api::{ActiveCouponReassignmentDelegation, CouponReassignmentDelegationSummary};

    /// The `CantonId` fields cross the wire as `prefix::namespace` strings and
    /// must parse back into the same id.
    #[test]
    fn canton_id_fields_round_trip() -> Result<()> {
        let net = NetworkInfo {
            dso_party_id: party("DSO")?,
            amulet_rules_cid: "00amulet".to_owned(),
            amulet_rules_blob: "blob".to_owned(),
        };
        let json = serde_json::to_string(&net)?;
        let back: NetworkInfo = serde_json::from_str(&json)?;
        assert_eq!(back.dso_party_id, net.dso_party_id);

        let services = ProviderServicesResponse {
            services: vec![ProviderServiceInfo {
                contract_id: "00svc".to_owned(),
                operator: party("op")?,
                provider: party("prov")?,
            }],
        };
        let json = serde_json::to_string(&services)?;
        let back: ProviderServicesResponse = serde_json::from_str(&json)?;
        assert_eq!(
            back.services.first().map(|s| &s.operator),
            Some(&party("op")?)
        );

        let delegations = ActiveCouponReassignmentDelegation {
            delegations: vec![CouponReassignmentDelegationSummary {
                cid: "00del".to_owned(),
                dso: party("DSO")?,
                assigners: vec![party("a1")?],
                beneficiary_count: 1,
            }],
        };
        let json = serde_json::to_string(&delegations)?;
        let back: ActiveCouponReassignmentDelegation = serde_json::from_str(&json)?;
        assert_eq!(
            back.delegations.first().map(|d| d.assigners.as_slice()),
            Some([party("a1")?].as_slice())
        );
        Ok(())
    }
}
