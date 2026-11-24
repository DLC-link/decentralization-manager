//! Generic responses, the audit log and the chain audit.

use serde::{Deserialize, Serialize};

use crate::types::{AuditLogEntry, WorkflowProgress};

/// Generic error response
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ErrorResponse {
    pub error: String,
}

/// Generic success response
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct MessageResponse {
    pub message: String,
}

/// Generic success boolean response
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct SuccessResponse {
    pub success: bool,
}

/// Response for workflow status check endpoints
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct WorkflowStatusResponse {
    pub status: WorkflowProgress,
    // Omit when absent so the wire matches the generated `error?: string`
    // (without `skip_serializing_if`, `None` serializes as `null`, which the
    // optional TS type does not accept).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Response for the governance audit endpoint
#[derive(Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct AuditLogResponse {
    pub entries: Vec<AuditLogEntry>,
    pub total_returned: usize,
}

/// A single on-chain governance audit entry
#[derive(Clone, Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ChainAuditEntry {
    /// Ledger offset (used for sort/pagination)
    pub offset: i64,
    /// Transaction effective_at in epoch seconds
    pub timestamp: i64,
    /// propose | confirm | execute | expire | cancel | create | other
    pub event_type: String,
    pub contract_id: String,
    /// "Module:Entity"
    pub template_id: String,
    pub package_id: String,
    /// core_self | core_domain | cbtc | unknown
    pub governance_type: String,
    pub action_summary: String,
    /// Exercised choice name (None for Created events)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub choice: Option<String>,
    /// Signatories (Created) or actingParties (Exercised)
    pub acting_parties: Vec<String>,
    pub update_id: String,
    /// Create arguments or choice argument as JSON
    #[cfg_attr(feature = "typegen", ts(type = "any"))]
    pub details: serde_json::Value,
}

/// Response for the on-chain governance audit endpoint
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ChainAuditResponse {
    pub entries: Vec<ChainAuditEntry>,
    pub total_returned: usize,
    /// Cursor for the next (older) page: pass back as `before_offset`. `None`
    /// when the page was short, i.e. there is nothing older left to read.
    pub next_before_offset: Option<i64>,
}
