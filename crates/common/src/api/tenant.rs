//! Wallet-held external parties: the hosted-party list and the tenant API
//! DTOs (wallet-facing `/v0/tenant/*`).
//!
//! Wallet-driven flow: the wallet generates and holds the Ed25519 key and DPM
//! relays. Every binary field on the wire is base64 (STANDARD engine).
//!
//! Both halves of each pair derive `Serialize` + `Deserialize`: the server reads
//! the requests and writes the responses, and the `decman-wallet` client does the
//! mirror image. One definition per message keeps the two ends from drifting.

use serde::{Deserialize, Serialize};

use crate::canton_id::CantonId;
use crate::types::{Permission, WorkflowProgress};

/// One external party this participant hosts, read from its own topology state
/// (an authorized `PartyToParticipant` mapping that names this participant with
/// Confirmation permission and whose namespace is a single self-signed key).
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ExternalPartyInfo {
    /// The party id (`{hint}::{fingerprint}`).
    pub party_id: String,
    /// The party's namespace fingerprint (the `{fingerprint}` half of the id).
    pub fingerprint: String,
    /// How many of the hosting participants must confirm a transaction involving
    /// this party (the M in M-of-N). Distinct from the party's *signing* threshold,
    /// which for a wallet-held party is 1 — one key authorizes, M hosts confirm.
    pub threshold: u32,
    /// How many participants host the party (the N in M-of-N).
    pub host_count: u32,
    /// When the hosting mapping became effective, RFC 3339.
    pub created_at: Option<String>,
    /// Whether this node still carries Canton's onboarding marker for the party.
    /// `true` means the party is hosted here and suspended here — it holds none
    /// of the party's contracts and confirms nothing yet.
    pub onboarding: bool,
    /// The participants named by the hosting mapping, so the UI can show *which*
    /// nodes host the party rather than only how many. Ordered as the mapping
    /// lists them.
    pub hosts: Vec<ExternalPartyHost>,
}

/// One participant named by an external party's `PartyToParticipant` mapping.
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ExternalPartyHost {
    /// The hosting participant's uid (`{hint}::{fingerprint}`).
    pub participant_uid: String,
    /// What the mapping lets this participant do for the party.
    pub permission: Permission,
}

/// Response wrapper for `GET /external-parties`.
#[derive(Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ExternalPartiesResponse {
    pub parties: Vec<ExternalPartyInfo>,
    /// When the topology scan behind `parties` finished, RFC 3339. `None`
    /// until the first scan finishes, and `parties` is then not yet known.
    pub fetched_at: Option<String>,
    /// Whether a topology scan is running now.
    pub refreshing: bool,
    /// Why the latest scan failed, when `parties` comes from an earlier one.
    pub refresh_error: Option<String>,
}

/// Request to prepare the onboarding topology for a wallet-held external party.
/// The wallet sends only its public key; DPM relays it to Canton and returns the
/// unsigned topology for the wallet to sign.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TenantPrepareRequest {
    /// Human-readable hint forming the party id's identifier segment.
    pub party_hint: String,
    /// The party's raw 32-byte Ed25519 public key, base64-encoded.
    pub public_key: String,
    /// Participants that will host the party (beyond this host).
    #[serde(default)]
    pub hosting_peers: Vec<CantonId>,
    /// Confirmation threshold for the hosting set. `None` defaults to `N-1` (one
    /// less than the number of hosting participants), never `N`, so a host can
    /// always exit later.
    #[serde(default)]
    pub confirmation_threshold: Option<u32>,
}

/// The unsigned onboarding topology for the wallet to sign. Every entry of
/// `transaction_hashes` and `topology_transactions` is base64-encoded, and the two
/// are index-aligned: `transaction_hashes[i]` is the hash to sign for
/// `topology_transactions[i]`.
///
/// The wallet signs each hash separately rather than one combined hash. Canton
/// hands us these hashes, so nothing here re-derives a Canton hash — and the
/// signatures go straight onto the transactions the node submits.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TenantPrepareResponse {
    /// The party id Canton derived (`{party_hint}::{fingerprint}`).
    pub party_id: String,
    /// The hash to sign for each transaction, base64-encoded, index-aligned with
    /// `topology_transactions`.
    pub transaction_hashes: Vec<String>,
    /// The serialized topology transactions, each base64-encoded.
    pub topology_transactions: Vec<String>,
}

/// Request to onboard a wallet-held external party from its signed topology.
/// `public_key` and every `topology_transactions` / `signatures` entry are
/// base64-encoded.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TenantExecuteRequest {
    pub party_hint: String,
    /// The party's raw Ed25519 public key, base64-encoded.
    pub public_key: String,
    /// The (unchanged) topology transactions from `TenantPrepareResponse`. The
    /// host set and confirmation threshold are carried inside these signed
    /// transactions, so they are not (and must not be) passed separately.
    pub topology_transactions: Vec<String>,
    /// One base64-encoded Ed25519 signature per transaction, index-aligned with
    /// `topology_transactions`: `signatures[i]` signs the hash the prepare step
    /// returned for `topology_transactions[i]`.
    pub signatures: Vec<String>,
    /// Fingerprint of the signing key (the `{fingerprint}` party-id segment).
    pub signed_by: String,
}

/// Response to a wallet onboarding request. Reports this host's view only: the
/// wallet calls `/execute` on every host and aggregates. `Completed` means this
/// host's authorized `PartyToParticipant` names it; `InProgress` means the
/// topology is still a proposal here (more hosts must sign).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TenantExecuteResponse {
    pub status: WorkflowProgress,
    pub party_id: String,
}

/// What permission a joining host gets.
///
/// Two shapes, and the plan calls them B1 and B2. `Confirmation` is the
/// co-validated one: several hosts confirm for the party and the threshold can
/// rise above 1. `Submission` is failover-only: any single host can submit for
/// the party, so uptime improves with no change to the party's application, but
/// each host acts alone.
///
/// Canton fixes the boundary between them — `topology.proto`: "if threshold > 1,
/// must be Confirmation or Observation" — so `Submission` is available only
/// while the threshold stays 1.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS))]
#[serde(rename_all = "lowercase")]
pub enum HostPermission {
    /// Co-validating. The default, and the only option once the threshold is
    /// above 1.
    #[default]
    Confirmation,
    /// Failover-only. Requires the party's threshold to be 1.
    Submission,
}

/// Request to prepare an add-hosts topology for an existing external party.
///
/// The wallet pins `base_serial` — the serial it read from the party's current
/// mapping — so every host builds the same serial N+1 transaction. A host whose
/// own view has moved on refuses rather than quietly preparing different bytes.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TenantAddHostsRequest {
    /// Full party id (`{hint}::{fingerprint}`) of the party gaining hosts.
    pub party_id: String,
    /// Permission the joining hosts get. Defaults to `Confirmation`.
    ///
    /// `Submission` is the failover-only offer: it needs no change to the
    /// party's application, because the host keeps submitting on its behalf
    /// rather than the party signing for itself. It is refused unless the
    /// party's threshold is 1, which is Canton's rule, not ours.
    #[serde(default)]
    pub permission: HostPermission,
    /// Participants to add. Each lands at `permission` with Canton's onboarding
    /// marker, and none may already host the party.
    pub new_hosts: Vec<CantonId>,
    /// The serial the wallet read from the party's current `PartyToParticipant`.
    pub base_serial: u32,
}

/// The unsigned add-hosts topology for the wallet to sign. Base64 and
/// index-aligned exactly as [`TenantPrepareResponse`].
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TenantAddHostsPrepareResponse {
    /// The party gaining hosts, echoed back.
    pub party_id: String,
    /// The serial the returned transactions carry (`base_serial + 1`).
    pub serial: u32,
    /// The hash to sign for each transaction, base64-encoded.
    pub transaction_hashes: Vec<String>,
    /// The serialized topology transactions, each base64-encoded.
    pub topology_transactions: Vec<String>,
}

/// Request to submit a wallet-signed add-hosts topology on one host.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TenantAddHostsExecuteRequest {
    /// Full party id of the party gaining hosts.
    pub party_id: String,
    /// The serial the wallet pinned when it prepared. Re-checked against this
    /// host's head state before anything is signed.
    pub base_serial: u32,
    /// The (unchanged) transactions from `TenantAddHostsPrepareResponse`. The
    /// host set rides inside them and must not be passed separately.
    pub topology_transactions: Vec<String>,
    /// One base64-encoded signature per transaction, index-aligned.
    pub signatures: Vec<String>,
    /// Fingerprint of the party key that produced the signatures.
    pub signed_by: String,
}

/// Response to an add-hosts submission. Reports this host's view only.
/// `Completed` means this host's authorized mapping already carries the new
/// hosts; `InProgress` means the change is still a proposal here.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TenantAddHostsExecuteResponse {
    pub status: WorkflowProgress,
    pub party_id: String,
    /// The serial this host now has for the party. Advances once the change is
    /// authorized; still the base serial while it is a proposal.
    pub serial: u32,
}

/// One block of the party's ACS, plus the packages needed to validate it.
///
/// The wallet is the transport: it reads block N here and hands block N to the
/// joining host, which feeds it straight into its open `ImportPartyAcs` stream.
/// Nothing lands on disk at either end, so the size of the party stops deciding
/// whether the transfer is possible — the same property the add-party path gets
/// from its node-to-node pipe.
///
/// Forward-only. The export stream cannot rewind, so a broken transfer restarts
/// from block 1 with a fresh export rather than resuming.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TenantAcsBlockResponse {
    /// The party the block belongs to.
    pub party_id: String,
    /// This block's sequence number, 1-based. Echoed so the wallet can prove a
    /// response belongs to the block it asked for.
    #[cfg_attr(feature = "typegen", ts(type = "number"))]
    pub seq: u64,
    /// The block's bytes, base64-encoded. Empty on the final block.
    pub chunk: String,
    /// Whether this is the final block. The wallet relays it like any other;
    /// the joiner treats it as the end of the stream.
    pub end: bool,
    /// Total bytes the export produced. Only meaningful when `end`.
    #[cfg_attr(feature = "typegen", ts(type = "number"))]
    pub total_len: u64,
    /// SHA-256 over everything the export produced, hex. Only meaningful when
    /// `end`, and what lets the joiner prove after the fact what it fed Canton.
    pub sha256: String,
    /// Package ids the joiner must have vetted before it can validate the
    /// snapshot. Repeated on every block so a wallet need not have kept the
    /// first response.
    pub package_ids: Vec<String>,
    /// Whether `package_ids` is trustworthy.
    ///
    /// The scan reads the party's contracts over the **Ledger** API, which needs
    /// a credential for that party. A node hosting an external party holds no
    /// such credential — the whole point is that the key is the wallet's — so on
    /// those deployments the scan cannot run and this is `false` with
    /// `package_ids` empty.
    ///
    /// `false` does not make the import unsafe: Canton re-validates every
    /// contract during `ImportPartyAcs` and fails on a missing package. It means
    /// that failure arrives *after* the joiner has disconnected rather than
    /// before, so an operator should confirm the joiner has the party's DARs
    /// vetted first.
    pub package_preflight: bool,
}

/// One relayed ACS block, handed to the joining host.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TenantAcsImportRequest {
    /// The party being replicated onto this host.
    pub party_id: String,
    /// The serial the add-hosts write was pinned to. Keys this replication, so
    /// a target that was removed and later re-added does not inherit the first
    /// attempt's state.
    pub base_serial: u32,
    /// This block's sequence number, 1-based. Block 1 starts a fresh import;
    /// anything else must be the block this host is waiting for.
    #[cfg_attr(feature = "typegen", ts(type = "number"))]
    pub seq: u64,
    /// The block's bytes, base64-encoded. Empty on the final block.
    pub chunk: String,
    /// Whether this is the final block.
    pub end: bool,
    /// The export's total length, from the source's final block.
    #[serde(default)]
    #[cfg_attr(feature = "typegen", ts(type = "number"))]
    pub total_len: u64,
    /// The export's SHA-256, from the source's final block.
    #[serde(default)]
    pub sha256: String,
    /// The package ids that came with it.
    #[serde(default)]
    pub package_ids: Vec<String>,
}

/// What the joining host did with a relayed block.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TenantAcsImportResponse {
    pub party_id: String,
    /// The block this host took. The call returns only once Canton has consumed
    /// it, which is what paces the wallet — neither end buffers.
    #[cfg_attr(feature = "typegen", ts(type = "number"))]
    pub seq: u64,
    /// Whether the final block landed and the import finished.
    pub complete: bool,
    /// Whether the snapshot was imported. `false` with `complete` means it was
    /// empty and the import was skipped, which is a success, not a failure.
    pub imported: bool,
    /// Whether Canton's onboarding marker is gone, i.e. the party is live on
    /// this host. `false` means the clearing transaction is proposed but not
    /// yet authorized — the party is hosted here but still suspended.
    pub marker_cleared: bool,
}

/// Request to prepare a confirmation-threshold change for an existing external
/// party. Separate from add-hosts on purpose: a newly added host cannot confirm
/// until its onboarding marker clears, so the two writes must not be combined.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TenantThresholdRequest {
    pub party_id: String,
    /// The threshold to move to. Must be between 1 and the number of hosts
    /// currently able to confirm — marked hosts do not count.
    pub new_threshold: u32,
    /// The serial the wallet read from the party's current mapping.
    pub base_serial: u32,
}

/// Request to submit a wallet-signed threshold change on one host.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TenantThresholdExecuteRequest {
    pub party_id: String,
    pub base_serial: u32,
    pub topology_transactions: Vec<String>,
    pub signatures: Vec<String>,
    pub signed_by: String,
}

/// Request to prepare a local party's conversion to an externally-signed one.
///
/// Only the node whose namespace owns the party can do this, and even it cannot
/// do it alone: the returned hash must be signed by the key being adopted.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct LocalPartyAdoptRequest {
    /// The local party. Its namespace must be this participant's.
    pub party_id: String,
    /// The raw 32-byte Ed25519 public key to adopt, base64-encoded.
    pub public_key: String,
    /// The serial the caller read from the party's current mapping.
    pub base_serial: u32,
}

/// Request to submit the owner-signed conversion.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct LocalPartyAdoptExecuteRequest {
    pub party_id: String,
    pub base_serial: u32,
    /// The adopted key, so the host can rebuild what the mapping must carry
    /// rather than trusting the submitted bytes.
    pub public_key: String,
    pub topology_transactions: Vec<String>,
    pub signatures: Vec<String>,
    pub signed_by: String,
}

/// A hosted party's current topology, as this host sees it.
///
/// Exists because `base_serial` is required by every write in this API and was
/// otherwise undiscoverable: a wallet has no Canton Admin API access, and
/// neither the status endpoint nor `/external-parties` reported the serial. The
/// writes were only usable by a caller that already knew something it had no way
/// to learn.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TenantPartyStateResponse {
    pub party_id: String,
    /// The authorized mapping's serial. Pin this as `base_serial` on the next
    /// write.
    pub serial: u32,
    /// How many hosts must confirm.
    pub threshold: u32,
    /// How many participants host the party.
    pub host_count: u32,
    /// Hosts still carrying Canton's onboarding marker, which confirm nothing
    /// until it clears. A threshold above `host_count - onboarding_hosts` is one
    /// the party cannot currently meet.
    pub onboarding_hosts: u32,
    /// Whether the party carries its own signing key, i.e. whether it is
    /// externally signed.
    ///
    /// This is the line the runbook routes on: a local party without one is a
    /// candidate for conversion, and one with it has already been converted (or
    /// was external from the start). Nothing else on the node reported it, so
    /// an operator learned which case they had by calling prepare and reading
    /// the 400.
    pub has_signing_key: bool,
}

/// A wallet provider's request that THIS host stop hosting an external party.
///
/// The host removes only itself, with its own key, so no party signature is
/// involved. Canton accepts that when the threshold is unchanged and exactly one
/// host leaves.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TenantUnhostRequest {
    /// Full party id of the party this host leaves.
    pub party_id: String,
    /// This host's own participant id. A request naming another participant is
    /// refused, so a caller cannot mistake which host it is talking to.
    pub participant_id: CantonId,
    /// The serial the caller read from the party's current mapping.
    pub base_serial: u32,
    /// Whether to archive the Splice contracts this host's validator holds for
    /// the party first. Omitted means yes. Send `false` only for a host that
    /// never acted as the party's Splice validator.
    pub splice_cleanup: Option<bool>,
}

/// An operator's request that this node stop hosting an external party. The
/// party is named in the path.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ExternalPartyUnhostRequest {
    /// The serial the preview reported.
    pub base_serial: u32,
    /// As on [`TenantUnhostRequest`]: omitted means yes.
    pub splice_cleanup: Option<bool>,
}

/// What an unhost attempt did, from either entry point.
///
/// `Completed` means this host's head state no longer lists it. `InProgress`
/// means the topology write was accepted but is not effective yet; repeat the
/// call to see it finish. `Failed` means a step failed after work started; the
/// audit row holds the reason.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ExternalPartyUnhostResponse {
    pub status: WorkflowProgress,
    pub party_id: String,
    /// The serial this host reads for the party now.
    pub serial: u32,
    /// `ValidatorRight` contracts this attempt archived.
    pub validator_rights_archived: Vec<String>,
    /// `TransferPreapproval` contracts this attempt cancelled.
    pub preapprovals_cancelled: Vec<String>,
    /// The audit row that records this attempt.
    pub audit_id: i64,
}

/// What an unhost would do on this node right now. Nothing is written.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct ExternalPartyUnhostPreview {
    pub party_id: String,
    /// The serial to pin as `base_serial` on the unhost.
    pub serial: u32,
    /// How many hosts must confirm. The unhost leaves it unchanged.
    pub threshold: u32,
    /// The hosts that remain once this node leaves.
    pub remaining_hosts: Vec<String>,
    /// This node's validator operator party, or `None` when
    /// `DECPM_VALIDATOR_OPERATOR_PARTY` is unset and no Splice cleanup can run.
    pub validator_operator_party: Option<String>,
    /// `ValidatorRight` contracts the unhost archives.
    pub validator_rights: Vec<String>,
    /// `TransferPreapproval` contracts the unhost cancels.
    pub preapprovals_to_cancel: Vec<String>,
    /// `TransferPreapproval` contracts past their expiry. The DSO expires
    /// these, so the unhost leaves them alone.
    pub preapprovals_expired: Vec<String>,
    /// Why the unhost would be refused, or `None` when it can go ahead.
    pub blocker: Option<String>,
}

/// One attempt to make this node leave an external party.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct UnhostAuditEntry {
    pub id: i64,
    pub party_id: String,
    pub participant_id: String,
    /// `operator` or `tenant`.
    pub requested_via: String,
    /// The admin user, or `tenant-key-N` for the Nth configured tenant key.
    pub requested_by: String,
    pub serial_before: Option<u32>,
    pub serial_after: Option<u32>,
    pub validator_rights_archived: Vec<String>,
    pub preapprovals_cancelled: Vec<String>,
    pub splice_cleanup: bool,
    /// The step the attempt reached: `preconditions`, `splice_cleanup`,
    /// `topology` or `done`.
    pub step: String,
    /// The earlier attempt this one resumed, if any.
    pub resumed_from: Option<i64>,
    /// Unix seconds.
    pub started_at: i64,
    /// Unix seconds. `None` while the attempt runs, or after a crash.
    pub finished_at: Option<i64>,
    /// `in_progress`, `completed` or `failed`.
    pub status: String,
    pub error: Option<String>,
}

/// Response wrapper for `GET /external-parties/{party_id}/unhost-audit`,
/// newest first.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct UnhostAuditResponse {
    pub entries: Vec<UnhostAuditEntry>,
}

/// One tenant API call that wrote topology or state on this node, or the end
/// of one ACS import.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TenantCallAuditEntry {
    pub id: i64,
    /// Unix seconds.
    pub called_at: i64,
    /// The index of the caller's key in `DECPM_TENANT_API_KEYS`. `None` in
    /// insecure mode, where no key is checked.
    pub key_index: Option<u32>,
    /// The first 16 hex characters of the SHA-256 of the caller's key. It
    /// still names the key after `DECPM_TENANT_API_KEYS` is reordered. `None`
    /// in insecure mode.
    pub key_id: Option<String>,
    /// The route that was called, such as `/v0/tenant/add-hosts/execute`. An
    /// ACS import row adds `(start)`, `(end)`, `(completed)` or `(abandoned)`
    /// to its route.
    pub endpoint: String,
    /// At most 256 characters. A longer value ends in `…[truncated]`.
    pub party_id: Option<String>,
    /// The HTTP status the call answered with. `None` for the end of an ACS
    /// import (`(completed)` or `(abandoned)`), which no call answers.
    pub http_status: Option<u16>,
    /// The error the call answered with, or why an import was abandoned.
    /// `None` on success. At most 1024 characters, like `party_id`.
    pub error: Option<String>,
}

/// Response wrapper for `GET /tenant-call-audit`, newest first.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[cfg_attr(feature = "typegen", derive(ts_rs::TS), ts(optional_fields))]
pub struct TenantCallAuditResponse {
    pub entries: Vec<TenantCallAuditEntry>,
}
