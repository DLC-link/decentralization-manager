import { fireEvent, render, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { SnackbarProvider } from "../contexts";
import type { ContractInfo, DecentralizedParty, PartyAuthStatus } from "../types";
import { PartyDetail } from "./PartyDetail";

// Every party view is a core-self governance party, so an owner's header
// button always reads "New Proposal". It opens the proposal form when the
// party has a rules contract, and the contract deployment dialog when not.
vi.mock("../api", () => ({
  authenticatedFetch: vi.fn((url: string) => {
    // The proposal form renders once the confirmations answer.
    if (url.includes("/governance/confirmations")) {
      return Promise.resolve({
        ok: true,
        json: async () => ({ rules_contract_id: "rules-1", threshold: 2, actions: [], domain_actions: [] }),
      });
    }
    // Every other fetch stays pending, so only the button's target is under test.
    return new Promise(() => {});
  }),
}));
vi.mock("../constants", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../constants")>()),
  ADMIN_ACCESS: true,
}));

const ns = "a".repeat(68);

const rulesContract: ContractInfo = {
  contract_id: "rules-1",
  template_id: "Governance.Rules:GovernanceRules",
  package_id: "gov-core-pkg",
  package_name: "governance-core",
  package_version: "0.1.0",
  created_at: "2026-01-01T00:00:00Z",
};

const ownedParty = (contracts: ContractInfo[]): DecentralizedParty => ({
  party_id: `alpha::${ns}`,
  threshold: 2,
  owners: ["owner-1"],
  my_owner_key: "owner-1",
  participants: [],
  non_hosting_owners: [],
  contracts,
});

const authStatus: PartyAuthStatus = {
  dec_party_id: `alpha::${ns}`,
  member_party_id: `member::${ns}`,
  user_id: "reviewer",
  status: { status: "authenticated" },
  rights: {
    member_party_act_as: true,
    member_party_read_as: true,
    dec_party_act_as: true,
    dec_party_read_as: true,
  },
};

const renderDetail = (party: DecentralizedParty) =>
  render(
    <SnackbarProvider>
      <PartyDetail
        party={party}
        onBack={() => {}}
        onRefresh={() => {}}
        onNavigateToNotifications={() => {}}
        authStatus={authStatus}
      />
    </SnackbarProvider>,
  );

// PartyDetail and GovernanceSection are large, so give each render room on a
// loaded CI runner.
describe("PartyDetail New Proposal button", { timeout: 20_000 }, () => {
  it("opens the proposal form when the party has a rules contract", async () => {
    renderDetail(ownedParty([rulesContract]));

    fireEvent.click(screen.getByRole("button", { name: "New Proposal" }));

    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByRole("heading", { name: "New Proposal" })).toBeTruthy();
    await within(dialog).findAllByRole("combobox", {}, { timeout: 5000 });
    expect(within(dialog).getAllByText("Proposal Type").length).toBeGreaterThan(0);
    expect(within(dialog).queryByText("Action Type")).toBeNull();
    expect(screen.queryByRole("heading", { name: "Deploy Contracts" })).toBeNull();
  });

  it("opens the contract deployment when the party has no rules contract", async () => {
    renderDetail(ownedParty([]));

    fireEvent.click(screen.getByRole("button", { name: "New Proposal" }));

    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByRole("heading", { name: "Deploy Contracts" })).toBeTruthy();
    expect(screen.queryByText("Proposal Type")).toBeNull();
  });
});
