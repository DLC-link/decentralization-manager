export interface AuthConfig {
  auth_required: boolean;
  keycloak_host?: string;
  keycloak_realm?: string;
  keycloak_client_id?: string;
  auth0_domain?: string;
  auth0_client_id?: string;
  auth0_audience?: string;
}

export interface ParticipantInfo {
  participant_uid: string;
  permission: string;
  owner_key?: string;
}

export interface ContractInfo {
  contract_id: string;
  template_id: string;
  package_id: string;
  package_name?: string;
  package_version?: string;
  created_at?: string;
}

export interface PackageConfig {
  governance_core?: string;
  governance_token_custody?: string;
  utility_credential?: string;
  utility_registry?: string;
  vault?: string;
  vault_governance?: string;
}

export interface PartyConfigRequest {
  dec_party_id: string;
  member_party_id: string;
  user_id: string;
  keycloak_url?: string;
  keycloak_realm?: string;
  keycloak_client_id?: string;
  keycloak_client_secret?: string;
  keycloak_username?: string;
  keycloak_password?: string;
  auth0_domain?: string;
  auth0_audience?: string;
  auth0_client_id?: string;
  auth0_client_secret?: string;
}

export interface PartyConfigResponse {
  dec_party_id: string;
  /** Optional: backend returns null when no config has been saved yet. */
  member_party_id?: string;
  /** Optional: backend returns null when no config has been saved yet. */
  user_id?: string;
  keycloak_url: string;
  keycloak_realm: string;
  keycloak_client_id: string;
  has_client_secret: boolean;
  has_username: boolean;
  has_password: boolean;
  auth0_domain?: string;
  auth0_audience?: string;
  auth0_client_id?: string;
  has_auth0_client_secret: boolean;
}

export interface VettedPackageInfo {
  package_id: string;
  package_name: string;
  package_version: string;
}

export interface PackageInfo {
  package_id: string;
  name: string;
  version: string;
}

export interface PeerPackageResult {
  participant_id: string;
  name: string;
  reachable: boolean;
  packages: PackageInfo[];
}

export interface PeerPackageComparison {
  local_packages: PackageInfo[];
  peers: PeerPackageResult[];
}

export interface PartyMetadata {
  annotations: Record<string, string>;
}

export interface DecentralizedParty {
  party_id: string;
  threshold: number;
  owners: string[];
  my_owner_key?: string;
  participants: ParticipantInfo[];
  contracts?: ContractInfo[];
  local_metadata?: PartyMetadata;
}

export type ResponseSource = "live" | "cache";

export interface DecentralizedPartiesResponse {
  parties: DecentralizedParty[];
  source?: ResponseSource;
  refreshing?: boolean;
}

export interface NodeConfig {
  node: {
    participant_id: string;
    listen_address: string;
    public_address?: string;
    port: number;
  };
  network_config: string;
  canton: {
    admin_api_host: string;
    admin_api_port: number;
    ledger_api_host: string;
    ledger_api_port: number;
    synchronizer: string;
    network: Network;
  };
  test_mode?: boolean;
  /** dec-party-manager binary version reported by the node. */
  version?: string;
}

export interface Peer {
  participant_id: string;
  name: string;
  address: string;
  port: number;
  public_key: string;
  party?: string;
}

export interface NetworkConfig {
  peers: Peer[];
}

export type ConnectionStatus =
  | "CurrentNode"
  | "Connected"
  | "Unreachable"
  | "HandshakeFailed";

export interface WorkflowInfo {
  kind: "Onboarding" | "Kick" | "Contracts" | "Dars";
  role: "Coordinator" | "Peer";
  step: string;
  step_index: number;
  step_total: number;
}

export interface ParticipantStatus {
  id: string;
  status: ConnectionStatus;
  latency_ms?: number;
  workflow?: WorkflowInfo;
  /** dec-party-manager version reported by the node (self or peer). */
  version?: string;
}

export interface KickRequest {
  decentralized_party_id: string;
  participant_id: string;
  new_threshold: number;
  /** Threshold before the kick — display-only, surfaced on the run card. */
  previous_threshold: number;
}

export type WorkflowProgress =
  | "idle"
  | "inprogress"
  | "completed"
  | "failed"
  | "cancelled";

// Type aliases for backwards compatibility
export type KickStatus = WorkflowProgress;
export type OnboardingStatus = WorkflowProgress;

export interface WorkflowStatusResponse {
  status: WorkflowProgress;
  error?: string;
}

// Type aliases for backwards compatibility
export type KickStatusResponse = WorkflowStatusResponse;
export type OnboardingStatusResponse = WorkflowStatusResponse;
export type ContractsStatusResponse = WorkflowStatusResponse;

export interface KeyStatusResponse {
  has_keys: boolean;
  public_key?: string;
}

// DAR file for upload
export interface DarFile {
  filename: string;
  data: string; // base64-encoded
}

// Contract deployment types
export type FieldDefinition =
  | { type: "decentralized_party" }
  | { type: "operator_party" }
  | { type: "participant_party"; id: string }
  | { type: "text"; value: string }
  | { type: "int64"; value: number }
  | { type: "bool"; value: boolean }
  | { type: "instrument"; id: string }
  | { type: "attestors_set" }
  | { type: "party_set"; parties: string[] }
  | { type: "rel_time"; microseconds: number }
  | { type: "optional"; inner: FieldDefinition }
  | { type: "record"; fields: FieldDefinition[] }
  | { type: "governance_threshold"; value?: number };

export interface ContractDefinition {
  id: string;
  name: string;
  package_id: string;
  module_name: string;
  entity_name: string;
  fields: FieldDefinition[];
  /** Optional UI-only labels for each field, by index. Backend ignores. */
  fieldLabels?: string[];
}

export interface ContractsRequest {
  decentralized_party_id: string;
  participant_ids: string[];
  participant_parties: string[];
  operator_party: string;
  contracts: ContractDefinition[];
}

export interface DarsRequest {
  dar_files: DarFile[];
}

export type DarsStatusResponse = WorkflowStatusResponse;

// Invitation types
export type InvitationType = "Onboarding" | "Kick" | "Contracts" | "Dars";

export interface PendingInvitation {
  id: string;
  workflow_instance?: string;
  invitation_type: InvitationType;
  coordinator_pubkey: string;
  coordinator_name?: string;
  received_at: number;
  /** Onboarding-only: party ID prefix the coordinator chose. */
  prefix?: string;
  /** Onboarding/Dars/Contracts: participant canton IDs the coordinator selected. */
  participants?: string[];
  /** Dars-only: filenames the coordinator is distributing. */
  dar_filenames?: string[];
  /** Kick-only: the participant being removed from the party. */
  kicked_participant?: string;
  /** Kick-only: threshold after the kick. */
  new_threshold?: number;
  /** Kick-only: threshold before the kick. */
  previous_threshold?: number;
  /** Kick/Contracts: the dec party the workflow targets. */
  dec_party_id?: string;
  /** Contracts-only: package/contract names being deployed. */
  package_names?: string[];
}

export interface PendingInvitationsResponse {
  invitations: PendingInvitation[];
}

// Workflow runs (live in-progress + recently-completed runs displayed in the
// notifications feed alongside pending invitations).
export type WorkflowKind = "Onboarding" | "Kick" | "Contracts" | "Dars";
export type WorkflowRole = "Coordinator" | "Peer";

export interface WorkflowRun {
  instance_name: string;
  kind: WorkflowKind;
  role: WorkflowRole;
  status: WorkflowProgress;
  current_step: string;
  step_index: number;
  step_total: number;
  /** Original config struct serialized to JSON — used for resume on the
   * backend; the frontend just treats this as opaque. */
  config_json: string;
  /** Hex-encoded coordinator Noise pubkey. None on coordinator-side rows. */
  coordinator_pubkey?: string;
  /** Resolved from the peers table when set. */
  coordinator_name?: string;
  expected_peers: string[];
  completed_peers: string[];
  dec_party_id?: string;
  prefix?: string;
  participants?: string[];
  dar_filenames?: string[];
  /** Contracts runs only: package/contract names being deployed. */
  package_names?: string[];
  /** Kick runs only: threshold before/after, for an "old → new" summary. */
  previous_threshold?: number;
  new_threshold?: number;
  /** Kick runs only: the participant being kicked. */
  kicked_participant?: string;
  error?: string;
  dismissed: boolean;
  created_at: number;
  updated_at: number;
}

export interface WorkflowRunsResponse {
  runs: WorkflowRun[];
}

// Authentication types
export type AuthStatus =
  | { status: "authenticated" }
  | { status: "mock" }
  | { status: "failed"; error: string }
  | { status: "notconfigured" };

export interface RightsStatus {
  member_party_act_as: boolean;
  member_party_read_as: boolean;
  dec_party_act_as: boolean;
  dec_party_read_as: boolean;
}

export interface PartyAuthStatus {
  dec_party_id: string;
  member_party_id: string;
  user_id: string;
  keycloak_url?: string;
  keycloak_realm?: string;
  status: AuthStatus;
  rights?: RightsStatus;
}

export interface AuthStatusResponse {
  parties: PartyAuthStatus[];
}

export interface AuthTestResult {
  party_id: string;
  success: boolean;
  error?: string;
}

export interface AuthTestResponse {
  results: AuthTestResult[];
}

export interface GrantRightsResponse {
  rights: RightsStatus;
}

// Governance types
export interface GovernanceConfirmation {
  contract_id: string;
  action: ActionType;
  confirming_party: string;
  /** Unix seconds when this confirmation contract was created on the ledger. */
  created_at?: number;
  /** Unix seconds of the confirmation's `expiresAt`. */
  expires_at?: number;
}

export interface GovernanceAction {
  action_hash: string;
  action: ActionType;
  confirmations: GovernanceConfirmation[];
  confirmation_count: number;
  can_execute: boolean;
  /** Unix seconds of the most recent confirmation. 0 if unresolved. */
  last_confirmation_at?: number;
}

export interface DomainGovernanceAction {
  proposal_cid: string;
  action_label: string;
  description?: string;
  confirmations: GovernanceConfirmation[];
  confirmation_count: number;
  can_execute: boolean;
  /** Underlying proposal contract is no longer in this participant's ACS;
   *  Confirmation contracts can only be expired (dismissed), not confirmed
   *  or executed. */
  orphaned?: boolean;
  /** Recipient / amount / instrument pulled from a TransferProposal so the
   *  notification card can show what's being transferred without an extra
   *  fetch. Present only on Transfer proposals. */
  transfer_details?: TransferProposalDetails;
  /** Sender / amount / instrument resolved from the TransferInstruction
   *  referenced by an AcceptTransferProposal so the pending-approval card
   *  shows who's sending what to whom. Present only on AcceptTransfer
   *  proposals (and only when the linked instruction was readable). */
  accept_transfer_details?: AcceptTransferDetails;
  /** Operator + user/provider parties pulled from a
   *  Create{User,Provider}ServiceRequest proposal so the pending-approval card
   *  shows the full summary (proposal type + operator + counterparty). Present
   *  only on those two proposal kinds. */
  service_request_details?: ServiceRequestDetails;
}

export interface ServiceRequestDetails {
  operator: string;
  /** Set for CreateUserServiceRequest. */
  user?: string;
  /** Set for CreateProviderServiceRequest. */
  provider?: string;
}

export interface TransferProposalDetails {
  receiver: string;
  amount: string;
  instrument_admin: string;
  instrument_id: string;
}

export interface AcceptTransferDetails {
  sender: string;
  receiver: string;
  amount: string;
  instrument_admin: string;
  instrument_id: string;
}

export interface GovernanceResponse {
  actions: GovernanceAction[];
  domain_actions?: DomainGovernanceAction[];
  threshold: number;
  member_party_id?: string;
  rules_contract_id?: string;
  gov_core_out_of_date?: boolean;
  gov_core_package_ref?: string;
}

export interface InstrumentAllowance {
  id: string;
}

export type ProposalType =
  | {
      type: "setup_cc_preapproval";
      provider: string;
      expected_dso: string;
    }
  | {
      type: "setup_token_preapproval";
      operator: string;
      instrument_admin: string;
      instrument_allowances: InstrumentAllowance[];
    }
  | {
      type: "transfer";
      transfer_factory_cid: string;
      expected_admin: string;
      receiver: string;
      amount: string;
      instrument_id: { admin: string; id: string };
      input_holding_cids: string[];
    }
  | {
      type: "accept_transfer";
      transfer_instruction_cid: string;
    }
  | {
      type: "generic_vote";
      description: string;
    }
  | {
      type: "provision_provider_service";
    }
  | {
      type: "setup_utility";
      provider_service_cid: string;
      operator: string;
      instrument_id_text: string;
      create_transfer_rule: boolean;
      create_allocation_factory: boolean;
    }
  | {
      type: "create_provider_service_request";
      operator: string;
      provider: string;
    }
  | {
      type: "create_user_service_request";
      operator: string;
      user: string;
    }
  | {
      type: "set_provider_app_reward_beneficiaries";
      instrument_configuration_cid: string;
      provider_app_reward_beneficiaries: AppRewardBeneficiary[] | null;
    }
  | {
      type: "set_enable_result_contracts";
      registrar_service_cid: string;
      enable_result_contracts: boolean | null;
    }
  | {
      type: "create_delegated_batched_markers_proxy";
      operator: string;
    }
  | {
      type: "mint";
      allocation_factory_cid: string;
      instrument_id: { admin: string; id: string };
      instrument_configuration_cid: string;
      recipient: string;
      amount: string;
      description: string;
    }
  | {
      type: "burn";
      allocation_factory_cid: string;
      instrument_id: { admin: string; id: string };
      instrument_configuration_cid: string;
      holder: string;
      amount: string;
      description: string;
    }
  | {
      type: "offer_free_credential";
      user_service_cid: string;
      holder: string;
      id: string;
      description: string;
      claims: Claim[];
    }
  | {
      type: "offer_paid_credential";
      user_service_cid: string;
      holder: string;
      id: string;
      description: string;
      claims: Claim[];
      billing_params: BillingParams;
      deposit_initial_amount_usd: string | null;
    }
  | {
      type: "accept_free_credential";
      user_service_cid: string;
      credential_offer_cid: string;
    }
  | {
      type: "accept_mint_request";
      mint_request_cid: string;
      instrument_configuration_cid: string;
      description: string;
    }
  | {
      type: "accept_burn_request";
      burn_request_cid: string;
      instrument_configuration_cid: string;
      description: string;
    };

export interface BillingParams {
  fee_per_day_usd: string;
  billing_period_minutes: number;
  deposit_target_amount_usd: string;
  holder_activity_weight: string | null;
}

export interface ProposeActionRequest {
  party_id: string;
  rules_contract_id: string;
  proposal: ProposalType;
}

// ============================================================================
// Structured Action Types for Vault Governance
// ============================================================================

export interface InstrumentId {
  admin: string;
  id: string;
}

export interface VaultLimits {
  max_total_deposit?: string;
  min_deposit_amount?: string;
  min_withdrawal_amount?: string;
}

export interface AppRewardBeneficiary {
  beneficiary: string;
  weight: string;
}

export interface FarConfig {
  featured_app_right_cid: string;
  beneficiaries: AppRewardBeneficiary[];
}

// Credential claim
export interface Claim {
  subject: string;
  property: string;
  value: string;
}

// Union type for all governance actions
export type ActionType =
  // Governance
  | {
      type: "governance_add_member";
      member: string;
      new_threshold: number;
    }
  | {
      type: "governance_remove_member";
      member: string;
      new_threshold: number;
    }
  | {
      type: "governance_set_threshold";
      new_threshold: number;
    }
  | {
      type: "governance_set_timeout";
      new_timeout_microseconds: number;
    }

  // Vault Deployment
  | {
      type: "vault_deployment";
      vault_rules_cid: string;
      vault_name: string;
      share_symbol: string;
      asset_instrument_id: InstrumentId;
      limits: VaultLimits;
      vault_backend_signatory: string;
      vault_far_config?: FarConfig;
      allocation_factory_cid: string;
      registrar_service_cid: string;
    }
  | {
      type: "yield_epoch_deployment";
      vault_rules_cid: string;
      vault_cid: string;
      asset_instrument_id: InstrumentId;
      vault_backend_signatory: string;
    }

  // Vault Operations
  | {
      type: "vault_pause";
      vault_id: string;
    }
  | {
      type: "vault_unpause";
      vault_id: string;
    }
  | {
      type: "vault_update_limits";
      vault_id: string;
      new_limits: VaultLimits;
    }
  | {
      type: "vault_update_backend";
      vault_id: string;
      new_backend_signatory: string;
    }
  | {
      type: "vault_update_far_beneficiaries";
      vault_id: string;
      new_beneficiaries: AppRewardBeneficiary[];
    }

  // Processor
  | {
      type: "processor_deployment_request";
      vault_processor_rules_cid: string;
      vault_backend_signatory: string;
      allocation_factory_cid: string;
      processor_far_config?: FarConfig;
      initial_supported_vaults: string[];
    }

  // Utility Onboarding
  | {
      type: "utility_create_provider_request";
      operator: string;
    }
  | {
      type: "utility_create_user_request";
      operator: string;
    }
  | {
      type: "utility_setup";
      operator: string;
      provider_service_cid: string;
      user_service_cid: string;
    }
  | {
      type: "utility_accept_holder_service_request";
      operator: string;
      provider_service_cid: string;
      holder_service_request_cid: string;
      holder: string;
    }
  // Credential Actions
  | {
      type: "credential_offer_free";
      operator: string;
      user_service_cid: string;
      holder: string;
      id: string;
      description: string;
      claims: Claim[];
    }
  | {
      type: "credential_accept_free";
      operator: string;
      user_service_cid: string;
      credential_offer_cid: string;
    }

  // DevNet
  | {
      type: "dev_net_feature_app";
      amulet_rules_cid: string;
    };

// Disclosed contract (contract_id + base64-encoded created_event_blob)
export interface DisclosedContract {
  contract_id: string;
  blob: string;
}

// Disclosed contract for ledger submission (same shape, used in requests)
export type DisclosedContractInput = DisclosedContract;

// Request types
export type GovernanceType = "vault" | "core_self" | "core_domain";

export interface ConfirmActionRequest {
  party_id: string;
  rules_contract_id: string;
  action: ActionType;
  governance_type?: GovernanceType;
}

export interface ExecuteActionRequest {
  party_id: string;
  rules_contract_id: string;
  action: ActionType;
  confirmation_cids: string[];
  disclosed_contracts: DisclosedContractInput[];
  governance_type?: GovernanceType;
}

export interface ExpireConfirmationRequest {
  party_id: string;
  rules_contract_id: string;
  confirmation_cid: string;
  governance_type?: GovernanceType;
}

export interface CancelConfirmationRequest {
  party_id: string;
  confirmation_cid: string;
  governance_type?: GovernanceType;
}

// Vault types
export interface VaultInfo {
  contract_id: string;
  vault_name: string;
  share_symbol: string;
  is_paused: boolean;
  vault_manager: string;
}

export interface VaultsResponse {
  vaults: VaultInfo[];
}

// Service types
export interface ProviderServiceInfo {
  contract_id: string;
  operator: string;
  provider: string;
}

export interface ProviderServicesResponse {
  services: ProviderServiceInfo[];
}

export interface UserServiceInfo {
  contract_id: string;
  operator: string;
  user: string;
}

export interface UserServicesResponse {
  services: UserServiceInfo[];
}

/** A pending CredentialOffer visible to the party. Free offers where the
 *  party is the holder are the ones the Accept Free Credential forms can
 *  take. */
export interface CredentialOfferInfo {
  contract_id: string;
  operator: string;
  issuer: string;
  holder: string;
  /** The credential's identifier (the template's `id` field). */
  credential_id: string;
  description: string;
  /** True when the offer carries no billing params (acceptable for free). */
  is_free: boolean;
}

export interface CredentialOffersResponse {
  credential_offers: CredentialOfferInfo[];
}

export interface RegistrarServiceInfo {
  contract_id: string;
  operator: string;
  registrar: string;
}

export interface RegistrarServicesResponse {
  services: RegistrarServiceInfo[];
}

export interface ContractWithBlob {
  contract_id: string;
  blob: string;
}

export interface ContractQueryResponse {
  contracts: ContractWithBlob[];
}

export interface NetworkInfo {
  dso_party_id: string;
  amulet_rules_cid: string;
  amulet_rules_blob: string;
}

export interface InstrumentInfo {
  contract_id: string;
  instrument_admin: string;
  instrument_id: string;
}

export interface InstrumentsResponse {
  instruments: InstrumentInfo[];
}

export type TransferInstructionStatus =
  | "pending_receiver_acceptance"
  | "pending_internal_workflow";

export interface PendingAction {
  party: string;
  action: string;
}

/** An open `TransferInstruction` whose `receiver` is this party. Includes
 *  offers blocked on an internal workflow so the dropdown can show them
 *  disabled with the "Pending: <party> — <action>" reason. */
export interface TransferInstructionInfo {
  contract_id: string;
  sender: string;
  receiver: string;
  amount: string;
  instrument_admin: string;
  instrument_id: string;
  status: TransferInstructionStatus;
  pending_actions?: PendingAction[];
  /** Unix seconds of the offer's `executeBefore` deadline. */
  expires_at?: number;
}

export interface TransferInstructionsResponse {
  transfer_instructions: TransferInstructionInfo[];
}

/** An open `MintRequest` / `BurnRequest` the governance party can accept.
 *  Shape is shared between mint and burn — the containing endpoint
 *  disambiguates. `expires_at` is unix seconds of `executeBefore`. */
export interface TokenRequestInfo {
  contract_id: string;
  holder: string;
  amount: string;
  instrument_admin: string;
  instrument_id: string;
  expires_at: number;
}

export interface MintRequestsResponse {
  mint_requests: TokenRequestInfo[];
}

export interface BurnRequestsResponse {
  burn_requests: TokenRequestInfo[];
}

/** Count of active TransferPreapproval contracts the gov party already has,
 *  split by direction (CC = Canton Coin via Splice; token = via Utility). */
export interface TransferPreapprovalsResponse {
  cc: number;
  token: number;
}

export interface Holding {
  instrument_admin: string;
  instrument_id: string;
  amount: string;
  preapproval_set_up: boolean;
}

export interface HoldingsResponse {
  holdings: Holding[];
}

export interface TransferFactoryInfo {
  contract_id: string;
  expected_admin: string;
}

export interface TransferFactoriesResponse {
  transfer_factories: TransferFactoryInfo[];
}

export interface GovernanceState {
  contract_id: string;
  vault_manager: string;
  members: string[];
  threshold: number;
  action_confirmation_timeout_microseconds?: number;
  package_ref?: string;
  out_of_date?: boolean;
}

export interface GovernanceStateResponse {
  state: GovernanceState | null;
}

export type Network = "devnet" | "testnet" | "mainnet";

// Governance audit trail types
export interface AuditLogEntry {
  id: number;
  timestamp: number;
  event_type: string;
  party_id: string;
  member_party_id: string;
  governance_type: string;
  action_summary: string;
  details: Record<string, unknown>;
  status: string;
  error_message?: string;
  created_at: number;
}

export interface AuditLogResponse {
  entries: AuditLogEntry[];
  total_returned: number;
}

export interface ChainAuditEntry {
  offset: number;
  timestamp: number;
  event_type: string;
  contract_id: string;
  template_id: string;
  package_id: string;
  governance_type: string;
  action_summary: string;
  choice?: string;
  acting_parties: string[];
  update_id: string;
  details: Record<string, unknown>;
}

export interface ChainAuditResponse {
  entries: ChainAuditEntry[];
  total_returned: number;
}
