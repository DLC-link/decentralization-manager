import type { ActionType, Network } from "./types";

export interface ActionTypeOption {
  value: ActionType["type"];
  label: string;
  hidden?: boolean;
}

// Action types — ordered per GOVERNANCE_CLIENT_MIGRATION.md vault launch sequence.
// Hidden entries are kept for type safety and display of existing actions.
export const getActionTypeOptions = (network?: Network): ActionTypeOption[] => [
  // Step 1: Utility Registry Onboarding
  {
    value: "utility_create_provider_request",
    label: "Create Provider Service",
  },
  { value: "utility_create_user_request", label: "Create User Service" },
  { value: "utility_setup", label: "Setup Utility" },
  // Feature App (needed before vault deployment for FAR config, devnet only)
  {
    value: "dev_net_feature_app",
    label: "DevNet: Feature App",
    hidden: network !== "devnet",
  },
  { value: "vault_deployment", label: "Deploy Vault" },
  { value: "yield_epoch_deployment", label: "Deploy YieldEpoch" },
  {
    value: "processor_deployment_request",
    label: "Request Processor Deployment",
  },
  {
    value: "utility_accept_holder_service_request",
    label: "Accept Holder Service",
    hidden: true,
  },
  {
    value: "governance_add_member",
    label: "Add Governance Member",
    hidden: true,
  },
  {
    value: "governance_remove_member",
    label: "Remove Governance Member",
    hidden: true,
  },
  {
    value: "governance_set_threshold",
    label: "Set Governance Threshold",
    hidden: true,
  },
  {
    value: "governance_set_timeout",
    label: "Set Governance Timeout",
    hidden: true,
  },
  { value: "vault_pause", label: "Pause Vault" },
  { value: "vault_unpause", label: "Unpause Vault" },
  { value: "vault_update_limits", label: "Update Vault Limits", hidden: true },
  {
    value: "vault_update_backend",
    label: "Update Vault Backend",
    hidden: true,
  },
  {
    value: "vault_update_far_beneficiaries",
    label: "Update FAR Beneficiaries",
    hidden: true,
  },
  {
    value: "credential_offer_free",
    label: "Credential: Offer Free",
    hidden: true,
  },
  {
    value: "credential_accept_free",
    label: "Credential: Accept Free",
  },
];

const ALL_ACTION_TYPE_OPTIONS = getActionTypeOptions();

export const formatActionType = (action: ActionType): string => {
  return (
    ALL_ACTION_TYPE_OPTIONS.find((opt) => opt.value === action.type)?.label ||
    action.type
  );
};

const truncatePartyId = (id: string): string => {
  const parts = id.split("::");
  if (parts.length !== 2) return id;
  const [prefix, namespace] = parts;
  return `${prefix}::${namespace.slice(0, 6)}…${namespace.slice(-6)}`;
};

export const formatMicroseconds = (us: number): string => {
  const ms = us / 1000;
  if (ms < 1000) return `${ms}ms`;
  const seconds = ms / 1000;
  if (seconds < 60) return `${seconds.toFixed(seconds < 10 ? 1 : 0)}s`;
  const minutes = seconds / 60;
  if (minutes < 60) return `${minutes.toFixed(1)} min`;
  const hours = minutes / 60;
  if (hours < 24) return `${hours.toFixed(1)} h`;
  return `${(hours / 24).toFixed(1)} d`;
};

export interface ActionDetail {
  label: string;
  before?: string;
  after: string;
}

/**
 * Build a small list of human-readable detail rows for an action.
 * Returns the most relevant fields per action type — typically the parameter
 * being changed and, when known, the current value the change is replacing.
 */
export const formatActionDetails = (
  action: ActionType,
  currentThreshold: number,
): ActionDetail[] => {
  switch (action.type) {
    case "governance_add_member":
      return [
        { label: "Member", after: truncatePartyId(action.member) },
        {
          label: "Threshold",
          before: String(currentThreshold),
          after: String(action.new_threshold),
        },
      ];
    case "governance_remove_member":
      return [
        { label: "Member", after: truncatePartyId(action.member) },
        {
          label: "Threshold",
          before: String(currentThreshold),
          after: String(action.new_threshold),
        },
      ];
    case "governance_set_threshold":
      return [
        {
          label: "Threshold",
          before: String(currentThreshold),
          after: String(action.new_threshold),
        },
      ];
    case "governance_set_timeout":
      return [
        {
          label: "Timeout",
          after: formatMicroseconds(action.new_timeout_microseconds),
        },
      ];
    case "vault_pause":
    case "vault_unpause":
      return [{ label: "Vault", after: truncatePartyId(action.vault_id) }];
    case "vault_update_backend":
      return [
        { label: "Vault", after: truncatePartyId(action.vault_id) },
        {
          label: "Backend",
          after: truncatePartyId(action.new_backend_signatory),
        },
      ];
    case "vault_update_far_beneficiaries":
      return [
        { label: "Vault", after: truncatePartyId(action.vault_id) },
        {
          label: "Beneficiaries",
          after: `${action.new_beneficiaries.length} entr${action.new_beneficiaries.length === 1 ? "y" : "ies"}`,
        },
      ];
    case "vault_update_limits":
      return [{ label: "Vault", after: truncatePartyId(action.vault_id) }];
    case "vault_deployment":
      return [
        { label: "Name", after: action.vault_name },
        { label: "Symbol", after: action.share_symbol },
      ];
    case "yield_epoch_deployment":
      return [{ label: "Vault", after: truncatePartyId(action.vault_cid) }];
    case "credential_offer_free":
      return [
        { label: "Holder", after: truncatePartyId(action.holder) },
        { label: "Credential", after: action.id },
      ];
    default:
      return [];
  }
};
