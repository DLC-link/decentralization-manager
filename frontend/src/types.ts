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
    ledger_api_user_id: string;
  };
}

export interface Peer {
  id: string;
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
  | { type: "participant_party"; index: number }
  | { type: "text"; value: string }
  | { type: "int64"; value: number }
  | { type: "bool"; value: boolean }
  | { type: "instrument"; id: string }
  | { type: "attestors_set" }
  | { type: "optional"; inner: FieldDefinition }
  | { type: "record"; fields: FieldDefinition[] }
  | { type: "governance_threshold" };

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
  operator_party?: string;
  operator_party_hint: string;
  dar_files: DarFile[];
  contracts: ContractDefinition[];
}
