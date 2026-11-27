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
});
