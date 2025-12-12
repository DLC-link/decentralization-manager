export interface ParticipantInfo {
  participant_uid: string;
  permission: string;
}

export interface ContractInfo {
  contract_id: string;
  template_id: string;
  package_id: string;
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
    node_id: string;
    listen_address: string;
  };
  network_config: string;
  canton: {
    admin_api_host: string;
    admin_api_port: number;
    ledger_api_host: string;
    ledger_api_port: number;
    synchronizer: string;
    ledger_api_user_id: string;
  };
}

export interface NetworkParticipant {
  id: string;
  name: string;
  role?: string;
  address: string;
  port: number;
  public_key: string;
}

export interface NetworkConfig {
  network: {
    name: string;
    protocol_version: string;
    port: number;
    coordinator_strategy: string;
  };
  participants: NetworkParticipant[];
}

export interface ParticipantStatus {
  id: string;
  active: boolean;
}

export interface KickRequest {
  decentralized_party_id: string;
  participant_id: string;
  namespace_fingerprint: string;
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

export interface KeyStatusResponse {
  has_keys: boolean;
  public_key?: string;
}

export interface KeygenResponse {
  success: boolean;
  public_key: string;
  message: string;
}
