export interface ParticipantInfo {
  participant_uid: string;
  permission: string;
}

export interface ContractInfo {
  contract_id: string;
  template_id: string;
  package_id: string;
}

export interface PackageConfig {
  vault_governance?: string;
  vault?: string;
  utility_registry?: string;
  utility_credential?: string;
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
  };
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

export interface ParticipantStatus {
  id: string;
  status: ConnectionStatus;
}

export interface KickRequest {
  decentralized_party_id: string;
  participant_id: string;
  namespace_fingerprint: string;
  new_threshold: number;
}

export type WorkflowProgress = "idle" | "inprogress" | "completed" | "failed";

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
  invitation_type: InvitationType;
  coordinator_pubkey: string;
  coordinator_name?: string;
  received_at: number;
}

export interface PendingInvitationsResponse {
  invitations: PendingInvitation[];
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

// Governance types
export interface GovernanceConfirmation {
  contract_id: string;
  action: ActionType;
  confirming_party: string;
}

export interface GovernanceAction {
  action_hash: string;
  action: ActionType;
  confirmations: GovernanceConfirmation[];
  confirmation_count: number;
  can_execute: boolean;
}

export interface GovernanceResponse {
  actions: GovernanceAction[];
  threshold: number;
  member_party_id?: string;
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

// Disclosed contract for ledger submission
export interface DisclosedContractInput {
  contract_id: string;
  blob: string; // base64-encoded created_event_blob
}

// Request types
export interface ConfirmActionRequest {
  party_id: string;
  rules_contract_id: string;
  action: ActionType;
}

export interface ExecuteActionRequest {
  party_id: string;
  rules_contract_id: string;
  action: ActionType;
  confirmation_cids: string[];
  disclosed_contracts: DisclosedContractInput[];
}

export interface ExpireConfirmationRequest {
  party_id: string;
  rules_contract_id: string;
  confirmation_cid: string;
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

export interface RegistrarServiceInfo {
  contract_id: string;
  operator: string;
  registrar: string;
}

export interface RegistrarServicesResponse {
  services: RegistrarServiceInfo[];
}

export interface ContractBlobResponse {
  contract_id: string;
  blob: string;
}
