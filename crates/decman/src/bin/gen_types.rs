//! Frontend TypeScript wire-type generator.
//!
//! Exports every `#[derive(ts_rs::TS)]` wire DTO — across `common::api`,
//! `common::types`, the `decman` server governance/decimal cluster, and the
//! node-config DTOs — into a single bundled file at
//! `crates/decman/frontend/src/types.generated.ts`.
//!
//! ts-rs's `export_all` walks each root type's dependency graph and writes one
//! file per type into a temp dir; this binary then concatenates them (dropping
//! the per-file `//` header and `import` lines, since everything lands in one
//! file) into the single bundle the frontend imports.
//!
//! Run via `cargo run -p decman --features typegen --bin gen-types`
//! (or `just gen-types`). The `typegen` feature is required — see Cargo.toml.

use std::{fs, path::PathBuf};

use ts_rs::{Config, TS};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = std::env::temp_dir().join("decman-ts-bindings");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp)?;
    // `with_large_int("number")` maps i64/u64/usize to `number` (matching the
    // wire, where serde emits them as JSON numbers). `with_out_dir` is where
    // `export_all` drops the per-type files.
    let cfg = Config::new().with_large_int("number").with_out_dir(&tmp);

    macro_rules! export_all {
        ($($t:ty),* $(,)?) => { $( <$t as TS>::export_all(&cfg)?; )* };
    }

    use dec_party_manager::{config, server};

    export_all!(
        // --- common::api: request/response + payload DTOs --------------------
        common::api::AuditLogResponse,
        common::api::AuthStatus,
        common::api::AuthStatusResponse,
        common::api::AuthTestResponse,
        common::api::AuthTestResult,
        common::api::CancelConfirmationRequest,
        common::api::ChainAuditEntry,
        common::api::ChainAuditResponse,
        common::api::Claim,
        common::api::ContractDefinition,
        common::api::ContractQueryResponse,
        common::api::ContractWithBlob,
        common::api::ContractsInvitePayload,
        common::api::ContractsRequest,
        common::api::CredentialOfferInfo,
        common::api::CredentialOffersResponse,
        common::api::DarFile,
        common::api::DarsInvitePayload,
        common::api::DarsRequest,
        common::api::DecentralizedPartiesResponse,
        common::api::DeclineInvitationPayload,
        common::api::DisclosedContractInput,
        common::api::DiscoverMemberPartyRequest,
        common::api::DiscoverMemberPartyResponse,
        common::api::ErrorResponse,
        common::api::ExpireConfirmationRequest,
        common::api::FieldDefinition,
        common::api::GovernanceState,
        common::api::GovernanceStateResponse,
        common::api::GovernanceType,
        common::api::GrantRightsRequest,
        common::api::GrantRightsResponse,
        common::api::InstrumentAllowance,
        common::api::InstrumentId,
        common::api::InstrumentIdentifier,
        common::api::InstrumentInfo,
        common::api::InstrumentsResponse,
        common::api::InvitationActionRequest,
        common::api::KeyStatusResponse,
        common::api::KickInvitePayload,
        common::api::KickRequest,
        common::api::KnownMember,
        common::api::KnownMembersResponse,
        common::api::MessageResponse,
        common::api::MissingEdgeKind,
        common::api::MissingPeerEdge,
        common::api::NetworkInfo,
        common::api::OnboardingInvitePayload,
        common::api::OnboardingMeshErrorResponse,
        common::api::OnboardingRequest,
        common::api::OperatorInfo,
        common::api::PackageConfig,
        common::api::PartyAuthStatus,
        common::api::PartyConfigRequest,
        common::api::PartyConfigResponse,
        common::api::PendingInvitationsResponse,
        common::api::ProviderServiceInfo,
        common::api::ProviderServicesResponse,
        common::api::RegistrarServiceInfo,
        common::api::RegistrarServicesResponse,
        common::api::ResponseSource,
        common::api::RightsStatus,
        common::api::SuccessResponse,
        common::api::TransferFactoriesResponse,
        common::api::TransferFactoryInfo,
        common::api::TransferPreapprovalsResponse,
        common::api::UserServiceInfo,
        common::api::UserServicesResponse,
        common::api::VaultInfo,
        common::api::VaultsResponse,
        common::api::WorkflowResponse,
        common::api::WorkflowRunsResponse,
        common::api::WorkflowStatusResponse,
        // --- common::types: shared building blocks ---------------------------
        common::types::AuditLogEntry,
        common::types::AuthConfigResponse,
        common::types::ConnectionStatus,
        common::types::ContractInfo,
        common::types::DecentralizedParty,
        common::types::InvitationType,
        common::types::PackageInfo,
        common::types::ParticipantInfo,
        common::types::ParticipantStatus,
        common::types::ParticipantsStatusResponse,
        common::types::PartyMetadata,
        common::types::PeerErrorKind,
        common::types::PeerPackageComparison,
        common::types::PeerPackageResult,
        common::types::PendingInvitation,
        common::types::Permission,
        common::types::VettedPackageInfo,
        common::types::WorkflowInfo,
        common::types::WorkflowKind,
        common::types::WorkflowProgress,
        common::types::WorkflowRole,
        common::types::WorkflowRun,
        // --- decman server::types: governance / decimal cluster --------------
        server::AcceptTransferDetails,
        server::ActionType,
        server::AppRewardBeneficiary,
        server::BillingParams,
        server::BurnRequestsResponse,
        server::ConfirmActionRequest,
        server::DomainGovernanceAction,
        server::ExecuteActionRequest,
        server::FarConfig,
        server::GovernanceAction,
        server::GovernanceConfirmation,
        server::GovernanceResponse,
        server::HoldingInfo,
        server::HoldingsResponse,
        server::MintRequestsResponse,
        server::PendingAction,
        server::ProposalType,
        server::ProposeActionRequest,
        server::ServiceRequestDetails,
        server::TokenRequestInfo,
        server::TransferInstructionInfo,
        server::TransferInstructionStatus,
        server::TransferInstructionsResponse,
        server::TransferProposalDetails,
        server::VaultLimits,
        // --- node-config DTOs ------------------------------------------------
        config::Auth0Config,
        config::CantonConfig,
        config::KeycloakConfig,
        config::Network,
        config::NetworkConfig,
        config::NodeConfig,
        config::NodeInfo,
        config::NoiseRetryConfig,
        config::Peer,
        config::Timeouts,
        server::NodeConfigResponse,
    );

    // Bundle: read every generated .ts file, drop ts-rs's `//` header lines and
    // the cross-file `import` lines (everything is in this one file), and
    // concatenate the decls.
    let mut blocks = Vec::new();
    let mut paths: Vec<PathBuf> = fs::read_dir(&tmp)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "ts"))
        .collect();
    paths.sort();
    for p in &paths {
        let content = fs::read_to_string(p)?;
        let body = content
            .lines()
            .filter(|l| {
                let t = l.trim_start();
                !t.starts_with("import ") && !t.starts_with("//")
            })
            .collect::<Vec<_>>()
            .join("\n");
        let body = body.trim();
        if !body.is_empty() {
            blocks.push(body.to_string());
        }
    }

    let header = "// Code generated from the Rust wire DTOs by `gen-types` (ts-rs).\n\
                  // DO NOT EDIT. Regenerate with `just gen-types`.\n\n";
    let dest = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("frontend/src/types.generated.ts");
    fs::write(&dest, format!("{header}{}\n", blocks.join("\n\n")))?;
    eprintln!("wrote {} ({} types)", dest.display(), blocks.len());
    Ok(())
}
