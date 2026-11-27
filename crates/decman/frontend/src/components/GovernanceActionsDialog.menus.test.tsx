import { fireEvent, render, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { SnackbarProvider } from "../contexts";
import type { Network } from "../types";
import { GovernanceActionsDialog } from "./GovernanceActionsDialog";

// The dialog is the only way the app opens GovernanceSection. These tests read
// the two type menus it offers, so they pin which forms a user can reach.
const ns = "a".repeat(68);
const self = `gov::${ns}`;

vi.mock("../api", () => ({
  authenticatedFetch: vi.fn((url: string) => {
    // GovernanceSection shows its forms once the confirmations answer.
    if (url.includes("/governance/confirmations")) {
      return Promise.resolve({
        ok: true,
        json: async () => ({ rules_contract_id: "rules-1", threshold: 2, actions: [], domain_actions: [] }),
      });
    }
    // Every other fetch stays pending, so only the menus are under test.
    return new Promise(() => {});
  }),
}));

// [type key the form submits, menu label]
const SELF_ACTIONS = [
  ["governance_add_member", "Add Governance Member"],
  ["governance_remove_member", "Remove Governance Member"],
  ["governance_set_threshold", "Set Governance Threshold"],
  ["governance_set_timeout", "Set Governance Timeout"],
  ["governance_set_disclosure_observers", "Set Disclosure Observers"],
];

const PROPOSAL_TYPES = [
  ["generic_vote", "Generic Vote"],
  ["setup_cc_preapproval", "Setup CC Preapproval"],
  ["setup_token_preapproval", "Setup Token Preapproval"],
  ["transfer", "Transfer"],
  ["accept_transfer", "Accept Transfer"],
  ["create_user_service_request", "1. Create User Service Request"],
  ["create_provider_service_request", "2. Create Provider Service Request"],
  ["setup_utility", "3. Setup Utility"],
  ["set_provider_app_reward_beneficiaries", "Set Provider App Reward Beneficiaries"],
  ["set_enable_result_contracts", "Set Enable Result Contracts"],
  ["update_instrument_configuration", "Update Instrument Configuration"],
  ["create_transfer_rule", "Create Transfer Rule"],
  ["create_delegated_batched_markers_proxy", "Create Delegated Batched Markers Proxy"],
  ["accept_mint_request", "Accept Mint Request"],
  ["accept_burn_request", "Accept Burn Request"],
  ["request_dev_net_featured_app_right", "Request DevNet Featured App Right"],
  ["create_user_service_request", "1. Create User Service Request (as Provider)"],
  ["create_provider_service_request", "2. Create Provider Service Request (as Provider)"],
  ["create_provider_configuration", "3. Create Provider Configuration (as Provider)"],
  ["create_registrar_service_request", "4. Create Registrar Service Request (as Registrar)"],
  ["onboard_registrar", "5. Onboard Registrar (as Provider)"],
  ["provision_instrument", "6. Provision Instrument (as Registrar)"],
  ["onboard_instrument_issuers", "Onboard Instrument Issuers (as Registrar)"],
  ["update_instrument_configuration", "Update Instrument Configuration (as Registrar)"],
  ["offboard_instrument_issuers", "Offboard Instrument Issuers (as Registrar)"],
  ["setup_minting_delegation", "Setup Minting Delegation"],
  ["accept_external_party_setup", "Accept External Party Setup"],
  ["setup_coupon_reassignment_delegation", "Setup Coupon Reassignment Delegation"],
  ["revoke_coupon_reassignment_delegation", "Revoke Coupon Reassignment Delegation"],
  ["offer_free_credential", "Offer Free Credential"],
  ["accept_free_credential", "Accept Free Credential"],
  ["offer_paid_credential", "Offer Paid Credential"],
  ["accept_paid_credential", "Accept Paid Credential"],
];

const renderDialog = (props: { network?: Network; view?: "actions" | "proposals" }) =>
  render(
    <SnackbarProvider>
      <GovernanceActionsDialog open onClose={() => {}} partyId={self} rulesContractId="rules-1" {...props} />
    </SnackbarProvider>,
  );

// The type select sits above every form's own fields, so it is the first
// combobox in the dialog.
const typeSelect = async () => (await screen.findAllByRole("combobox", {}, { timeout: 5000 }))[0];

// Open the type select and read each choosable entry. MUI gives the group
// headers and dividers the option role too, but only entries carry a value.
const menuEntries = async () => {
  fireEvent.mouseDown(await typeSelect());
  return within(screen.getByRole("listbox"))
    .getAllByRole("option")
    .filter((option) => option.hasAttribute("data-value"))
    .map((option) => [option.getAttribute("data-value"), option.textContent]);
};

// GovernanceSection is large, so give each render room on a loaded CI runner.
describe("governance dialog menus", { timeout: 20_000 }, () => {
  it.each<Network | undefined>(["devnet", "testnet", "mainnet", undefined])(
    "offers only the five self actions on network %s",
    async (network) => {
      renderDialog({ network });

      expect(screen.getByRole("heading", { name: "Governance Actions" })).toBeTruthy();
      // The form opens on a self action, never on a utility action.
      expect((await typeSelect()).textContent).toBe("Add Governance Member");
      // No utility, credential or DevNet action form can be chosen.
      expect(await menuEntries()).toEqual(SELF_ACTIONS);
    },
  );

  // The full list, so a mint, burn or provision_provider_service entry, or
  // any other change to the menu, fails here.
  it("lists the same proposal types, with no mint, burn or provision entry", async () => {
    renderDialog({ view: "proposals" });

    expect(await menuEntries()).toEqual(PROPOSAL_TYPES);
  });
});
