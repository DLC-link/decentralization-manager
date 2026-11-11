import type { ProposalType } from "../../../types";

// The proposal types the type menu offers. The API also accepts mint, burn
// and provision_provider_service, but this form never builds them.
type FormProposalType = Exclude<
  ProposalType["type"],
  "mint" | "burn" | "provision_provider_service"
>;

/** One row of the proposal type menu, in display order. */
type ProposalMenuEntry =
  | { kind: "group"; label: string }
  | { kind: "step"; label: string }
  | { kind: "type"; value: FormProposalType; label: string }
  | { kind: "divider" };

// A "group" is a bold section header and a "step" an italic sub-header.
// A proposal type may appear in more than one group.
const PROPOSAL_MENU: ProposalMenuEntry[] = [
  { kind: "group", label: "Governance Core" },
  { kind: "type", value: "generic_vote", label: "Generic Vote" },
  { kind: "divider" },
  { kind: "group", label: "Token Custody" },
  { kind: "type", value: "setup_cc_preapproval", label: "Setup CC Preapproval" },
  { kind: "type", value: "setup_token_preapproval", label: "Setup Token Preapproval" },
  { kind: "type", value: "transfer", label: "Transfer" },
  { kind: "type", value: "accept_transfer", label: "Accept Transfer" },
  { kind: "divider" },
  { kind: "group", label: "Utility Onboarding" },
  { kind: "step", label: "Onboarding (in order)" },
  { kind: "type", value: "create_user_service_request", label: "1. Create User Service Request" },
  { kind: "type", value: "create_provider_service_request", label: "2. Create Provider Service Request" },
  { kind: "type", value: "setup_utility", label: "3. Setup Utility" },
  { kind: "step", label: "Settings / Configuration" },
  { kind: "type", value: "set_provider_app_reward_beneficiaries", label: "Set Provider App Reward Beneficiaries" },
  { kind: "type", value: "set_enable_result_contracts", label: "Set Enable Result Contracts" },
  { kind: "type", value: "update_instrument_configuration", label: "Update Instrument Configuration" },
  { kind: "type", value: "create_transfer_rule", label: "Create Transfer Rule" },
  { kind: "type", value: "create_delegated_batched_markers_proxy", label: "Create Delegated Batched Markers Proxy" },
  { kind: "step", label: "Actions" },
  { kind: "type", value: "accept_mint_request", label: "Accept Mint Request" },
  { kind: "type", value: "accept_burn_request", label: "Accept Burn Request" },
  { kind: "step", label: "DevNet" },
  { kind: "type", value: "request_dev_net_featured_app_right", label: "Request DevNet Featured App Right" },
  { kind: "divider" },
  { kind: "group", label: "Dual Governance Utility Onboarding" },
  { kind: "step", label: "Onboarding (in order)" },
  { kind: "type", value: "create_user_service_request", label: "1. Create User Service Request (as Provider)" },
  { kind: "type", value: "create_provider_service_request", label: "2. Create Provider Service Request (as Provider)" },
  { kind: "type", value: "create_provider_configuration", label: "3. Create Provider Configuration (as Provider)" },
  { kind: "type", value: "create_registrar_service_request", label: "4. Create Registrar Service Request (as Registrar)" },
  { kind: "type", value: "onboard_registrar", label: "5. Onboard Registrar (as Provider)" },
  { kind: "type", value: "provision_instrument", label: "6. Provision Instrument (as Registrar)" },
  { kind: "step", label: "Instrument Management" },
  { kind: "type", value: "onboard_instrument_issuers", label: "Onboard Instrument Issuers (as Registrar)" },
  { kind: "type", value: "update_instrument_configuration", label: "Update Instrument Configuration (as Registrar)" },
  { kind: "type", value: "offboard_instrument_issuers", label: "Offboard Instrument Issuers (as Registrar)" },
  { kind: "divider" },
  { kind: "group", label: "Rewards" },
  { kind: "type", value: "setup_minting_delegation", label: "Setup Minting Delegation" },
  { kind: "type", value: "accept_external_party_setup", label: "Accept External Party Setup" },
  { kind: "type", value: "setup_coupon_reassignment_delegation", label: "Setup Coupon Reassignment Delegation" },
  { kind: "type", value: "revoke_coupon_reassignment_delegation", label: "Revoke Coupon Reassignment Delegation" },
  { kind: "divider" },
  { kind: "group", label: "Utility Credential" },
  { kind: "type", value: "offer_free_credential", label: "Offer Free Credential" },
  { kind: "type", value: "accept_free_credential", label: "Accept Free Credential" },
  { kind: "type", value: "offer_paid_credential", label: "Offer Paid Credential" },
  { kind: "type", value: "accept_paid_credential", label: "Accept Paid Credential" },
];

export { PROPOSAL_MENU };
export type { FormProposalType, ProposalMenuEntry };
