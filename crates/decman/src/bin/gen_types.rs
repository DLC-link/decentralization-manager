//! Frontend TypeScript wire-type generator.
//!
//! Exports every `#[derive(ts_rs::TS)]` wire DTO into one bundled file at
//! `crates/decman/frontend/src/types.generated.ts`. `export_all` walks each
//! type's dependency graph and writes one file per type into a temp dir; this
//! binary then concatenates them (dropping the per-file `//` header and
//! cross-file `import` lines, since everything lands in one file).
//!
//! Run via `just gen-types` (`cargo run -p decman --features typegen --bin gen-types`).

use std::{fs, path::PathBuf};

use ts_rs::{Config, TS};

// The wire DTOs live across `common` (api + shared types) and `decman`
// (governance/decimal cluster + node config). Glob the `common` modules; the
// `decman`-only types are imported by name (a glob over `server` would clash
// with its re-exports of `common`).
use common::{api::*, types::*};
use dec_party_manager::{
    config::{
        Auth0Config, CantonConfig, KeycloakConfig, Network, NetworkConfig, NodeConfig, NodeInfo,
        NoiseRetryConfig, Peer, Timeouts,
    },
    server::{
        AcceptTransferDetails, ActionType, AppRewardBeneficiary, BillingParams, BurnRequestsResponse,
        ConfirmActionRequest, DomainGovernanceAction, ExecuteActionRequest, FarConfig,
        GovernanceAction, GovernanceConfirmation, GovernanceResponse, HoldingInfo, HoldingsResponse,
        MintRequestsResponse, NodeConfigResponse, PendingAction, ProposalType, ProposeActionRequest,
        ServiceRequestDetails, TokenRequestInfo, TransferInstructionInfo, TransferInstructionStatus,
        TransferInstructionsResponse, TransferProposalDetails, VaultLimits,
    },
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = std::env::temp_dir().join("decman-ts-bindings");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp)?;
    // `with_large_int("number")` maps i64/u64/usize to `number` (the wire emits
    // them as JSON numbers); `with_out_dir` is where `export_all` drops files.
    let cfg = Config::new().with_large_int("number").with_out_dir(&tmp);

    // Export every wire type. `export_all` also pulls each type's dependencies,
    // and re-exporting a dependency is a no-op, so the list need not be minimal.
    macro_rules! export {
        ($($t:ty),* $(,)?) => { $( <$t as TS>::export_all(&cfg)?; )* };
    }
    export![
        AuditLogEntry, AuditLogResponse, AuthConfigResponse, AuthStatus, AuthStatusResponse,
        AuthTestResponse, AuthTestResult, CancelConfirmationRequest, ChainAuditEntry,
        ChainAuditResponse, Claim, ConnectionStatus, ContractDefinition, ContractInfo,
        ContractQueryResponse, ContractWithBlob, ContractsInvitePayload, ContractsRequest,
        CredentialOfferInfo, CredentialOffersResponse, DarFile, DarsInvitePayload, DarsRequest,
        DecentralizedPartiesResponse, DecentralizedParty, DeclineInvitationPayload,
        DisclosedContractInput, DiscoverMemberPartyRequest, DiscoverMemberPartyResponse,
        ErrorResponse, ExpireConfirmationRequest, FieldDefinition, GovernanceState,
        GovernanceStateResponse, GovernanceType, GrantRightsRequest, GrantRightsResponse,
        InstrumentAllowance, InstrumentId, InstrumentIdentifier, InstrumentInfo, InstrumentsResponse,
        InvitationActionRequest, InvitationType, KeyStatusResponse, KickInvitePayload, KickRequest,
        KnownMember, KnownMembersResponse, MessageResponse, MissingEdgeKind, MissingPeerEdge,
        NetworkInfo, OnboardingInvitePayload, OnboardingMeshErrorResponse, OnboardingRequest,
        OperatorInfo, PackageConfig, PackageInfo, ParticipantInfo, ParticipantStatus,
        ParticipantsStatusResponse, PartyAuthStatus, PartyConfigRequest, PartyConfigResponse,
        PartyMetadata, PeerErrorKind, PeerPackageComparison, PeerPackageResult, PendingInvitation,
        PendingInvitationsResponse, Permission, ProviderServiceInfo, ProviderServicesResponse,
        RegistrarServiceInfo, RegistrarServicesResponse, ResponseSource, RightsStatus,
        SuccessResponse, TransferFactoriesResponse, TransferFactoryInfo, TransferPreapprovalsResponse,
        UserServiceInfo, UserServicesResponse, VaultInfo, VaultsResponse, VettedPackageInfo,
        WorkflowInfo, WorkflowKind, WorkflowProgress, WorkflowResponse, WorkflowRole, WorkflowRun,
        WorkflowRunsResponse, WorkflowStatusResponse,
        // decman governance / decimal cluster
        AcceptTransferDetails, ActionType, AppRewardBeneficiary, BillingParams, BurnRequestsResponse,
        ConfirmActionRequest, DomainGovernanceAction, ExecuteActionRequest, FarConfig,
        GovernanceAction, GovernanceConfirmation, GovernanceResponse, HoldingInfo, HoldingsResponse,
        MintRequestsResponse, PendingAction, ProposalType, ProposeActionRequest,
        ServiceRequestDetails, TokenRequestInfo, TransferInstructionInfo, TransferInstructionStatus,
        TransferInstructionsResponse, TransferProposalDetails, VaultLimits,
        // node config DTOs
        Auth0Config, CantonConfig, KeycloakConfig, Network, NetworkConfig, NodeConfig,
        NodeConfigResponse, NodeInfo, NoiseRetryConfig, Peer, Timeouts,
    ];

    // Bundle: concatenate every generated file, dropping ts-rs's `//` header and
    // the cross-file `import` lines (everything is in this one file).
    let mut paths: Vec<PathBuf> = fs::read_dir(&tmp)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "ts"))
        .collect();
    paths.sort();
    let mut blocks = Vec::new();
    for p in &paths {
        let body = fs::read_to_string(p)?
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                !t.starts_with("import ") && !t.starts_with("//")
            })
            .collect::<Vec<_>>()
            .join("\n");
        if !body.trim().is_empty() {
            blocks.push(body.trim().to_string());
        }
    }

    let header = "// Code generated from the Rust wire DTOs by `gen-types` (ts-rs).\n\
                  // DO NOT EDIT. Regenerate with `just gen-types`.\n\n";
    let dest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("frontend/src/types.generated.ts");
    fs::write(&dest, format!("{header}{}\n", blocks.join("\n\n")))?;
    eprintln!("wrote {} ({} types)", dest.display(), blocks.len());
    Ok(())
}
