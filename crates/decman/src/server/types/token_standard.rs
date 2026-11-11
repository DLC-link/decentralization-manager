//! Token-standard contract views.

use canton_common::decimal::DamlDecimal;
use decman_lib::catalog::types::BillingParams;
use serde::Serialize;

use crate::canton_id::CantonId;

/// An open `TransferInstruction` whose `receiver` is this party. Includes
/// offers waiting on an internal workflow (admin / registrar) so the dropdown
/// can surface them as "pending: X" rather than silently hide them.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TransferInstructionInfo {
    pub contract_id: String,
    pub sender: CantonId,
    pub receiver: CantonId,
    #[schema(value_type = String)]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    pub amount: DamlDecimal,
    pub instrument_admin: CantonId,
    pub instrument_id: String,
    pub status: TransferInstructionStatus,
    /// For `PendingInternalWorkflow`: the parties whose action is awaited and
    /// the human-readable label of what they need to do.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_actions: Vec<PendingAction>,
    /// Unix seconds of the offer's `executeBefore` deadline. Past-deadline
    /// rows are surfaced anyway (disabled in the UI) so the user can see they
    /// exist — Daml refuses to Accept them, but staying silent confused users.
    #[serde(default)]
    pub expires_at: i64,
}

/// One row of `TransferInstructionStatus.pendingActions`. The Daml type is
/// `Map Party Text`; the receiver can render "<party> — <action>" per row.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct PendingAction {
    pub party: CantonId,
    pub action: String,
}

/// Mirrors `Splice.Api.Token.TransferInstructionV1.TransferInstructionStatus`.
#[derive(Clone, Copy, Debug, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
#[serde(rename_all = "snake_case")]
pub enum TransferInstructionStatus {
    PendingReceiverAcceptance,
    PendingInternalWorkflow,
}

/// Response for the transfer instructions endpoint.
#[derive(Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TransferInstructionsResponse {
    pub transfer_instructions: Vec<TransferInstructionInfo>,
}

/// An open `MintRequest`/`BurnRequest` (`Utility.Registry.App.V0.Model.{Mint,Burn}`)
/// the governance party can accept. The shape is identical for both kinds; the
/// containing endpoint disambiguates. `expires_at` is read off the inner
/// `mint`/`burn` payload's `executeBefore` field so the dropdown can disable
/// past-deadline rows.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TokenRequestInfo {
    pub contract_id: String,
    pub holder: CantonId,
    #[schema(value_type = String)]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    pub amount: DamlDecimal,
    pub instrument_admin: CantonId,
    pub instrument_id: String,
    /// Unix seconds of the request's `executeBefore` deadline.
    pub expires_at: i64,
}

/// Response for the mint-requests endpoint.
#[derive(Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct MintRequestsResponse {
    pub mint_requests: Vec<TokenRequestInfo>,
}

/// Response for the burn-requests endpoint.
#[derive(Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct BurnRequestsResponse {
    pub burn_requests: Vec<TokenRequestInfo>,
}

/// A pending `Utility.Credential.App.V0.Model.Offer:CredentialOffer` visible
/// to the party. The accept credential forms list offers where the party is
/// the `holder`, so the CredentialOffer cid no longer has to be pasted in by
/// hand.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct CredentialOfferInfo {
    pub contract_id: String,
    pub operator: CantonId,
    pub issuer: CantonId,
    pub holder: CantonId,
    /// The template's `id` field — the credential's identifier.
    pub credential_id: String,
    pub description: String,
    /// True when the offer carries no `billingParams`, i.e. it can be taken
    /// with `CredentialOffer_AcceptFree` (the only direction the
    /// AcceptFreeCredential governance action supports).
    pub is_free: bool,
    /// The offer's billing params. A free offer has none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub billing_params: Option<BillingParams>,
    /// The USD amount the offer locks at accept instead of its deposit target.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>)]
    #[cfg_attr(feature = "typegen", ts(as = "Option<String>"))]
    pub deposit_initial_amount_usd: Option<DamlDecimal>,
}

/// Response for the credential offers endpoint
#[derive(Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct CredentialOffersResponse {
    pub credential_offers: Vec<CredentialOfferInfo>,
}

/// A token-standard Holding owned by a decentralized party, aggregated across
/// every active `Splice.Api.Token.HoldingV1:Holding` contract that shares the
/// same `(instrument_admin, instrument_id)` pair.
#[derive(Clone, Debug, Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct HoldingInfo {
    pub instrument_admin: CantonId,
    pub instrument_id: String,
    /// Total amount held, summed across every active `Holding` contract for
    /// this instrument — including locked ones.
    #[schema(value_type = String)]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    pub amount: DamlDecimal,
    /// Portion of `amount` that is locked (escrowed for an in-flight
    /// transfer/allocation) and therefore not freely transferable. The
    /// available balance is `amount - locked_amount`.
    #[schema(value_type = String)]
    #[cfg_attr(feature = "typegen", ts(type = "string"))]
    pub locked_amount: DamlDecimal,
    /// True if a `TransferPreapproval` is in place for this party for this
    /// instrument. CC (Amulet) holdings match when any
    /// `Splice.AmuletRules:TransferPreapproval` exists; utility-token holdings
    /// match by `(instrument_admin, instrument_id)` against
    /// `Utility.Registry.App.V0.Model.TransferPreapproval` contracts.
    pub preapproval_set_up: bool,
}

/// Response for the holdings endpoint.
#[derive(Serialize, utoipa::ToSchema)]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct HoldingsResponse {
    pub holdings: Vec<HoldingInfo>,
}
