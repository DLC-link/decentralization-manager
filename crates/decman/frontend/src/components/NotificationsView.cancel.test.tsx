import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { SnackbarProvider } from "../contexts";
import type { DomainGovernanceAction, GovernanceAction } from "../types";
import { NotificationsView } from "./NotificationsView";

// Members cancel a proposal whose proposer the rules no longer authorize by
// voting the SelfAction_CancelAction self-action (#518).
const partyId = `shared::1220${"a".repeat(64)}`;
const memberId = `member::1220${"b".repeat(64)}`;
const formerAgentId = `agent::1220${"c".repeat(64)}`;

const { authenticatedFetch } = vi.hoisted(() => ({ authenticatedFetch: vi.fn() }));
vi.mock("../api", () => ({ authenticatedFetch }));

beforeEach(() => {
  authenticatedFetch.mockReset();
  authenticatedFetch.mockResolvedValue({ ok: true, json: async () => ({}) });
});

const proposal = (overrides: Partial<DomainGovernanceAction> = {}): DomainGovernanceAction => ({
  proposal_cid: "00stranded",
  action_label: "Transfer",
  description: "Pay the seat fee to the former agent",
  confirmations: [],
  confirmation_count: 0,
  executable_confirmation_cids: [],
  can_execute: false,
  orphaned: false,
  proposer: formerAgentId,
  proposer_not_authorized: true,
  created_at: 1_700_000_000,
  ...overrides,
});

const cancellation = (proposalCid: string): GovernanceAction => ({
  action_hash: "cancel-hash",
  action: { type: "governance_cancel_action", proposal_cid: proposalCid },
  confirmations: [
    {
      contract_id: "00selfconf",
      action: { type: "governance_cancel_action", proposal_cid: proposalCid },
      confirming_party: `other::1220${"d".repeat(64)}`,
      created_at: 1_700_000_100,
      expires_at: 4_000_000_000,
    },
  ],
  confirmation_count: 1,
  executable_confirmation_cids: ["00selfconf"],
  can_execute: false,
  last_confirmation_at: 1_700_000_100,
});

const renderFeed = (
  domainActions: DomainGovernanceAction[],
  actions: GovernanceAction[] = [],
) =>
  render(
    <SnackbarProvider>
      <NotificationsView
        pendingInvitations={[]}
        partyActions={[
          {
            partyId,
            rulesContractId: "00rules",
            memberPartyId: memberId,
            governanceType: "core_self",
            threshold: 2,
            actions,
            domainActions,
          },
        ]}
        workflowRuns={[]}
        loading={false}
        onInvitationsChanged={() => {}}
        onActionsChanged={() => {}}
        onWorkflowsChanged={() => {}}
        onSelectParty={() => {}}
      />
    </SnackbarProvider>,
  );

const confirmPosts = () =>
  authenticatedFetch.mock.calls.filter(([url]) => String(url).endsWith("/governance/confirm"));

describe("cancelling a stranded proposal", () => {
  it("lets a member propose the cancellation after seeing what it cancels", async () => {
    renderFeed([proposal()]);

    fireEvent.click(screen.getByRole("button", { name: "Propose cancellation" }));
    const dialog = screen.getByRole("dialog");
    expect(within(dialog).getByText("Transfer")).toBeTruthy();
    expect(within(dialog).getByText(formerAgentId)).toBeTruthy();
    expect(within(dialog).getByText("Pay the seat fee to the former agent")).toBeTruthy();
    expect(within(dialog).getByText("00stranded")).toBeTruthy();
    expect(confirmPosts()).toHaveLength(0);

    fireEvent.click(within(dialog).getByRole("button", { name: "Propose cancellation" }));

    await waitFor(() => expect(confirmPosts()).toHaveLength(1));
    expect(JSON.parse(String(confirmPosts()[0][1]?.body))).toEqual({
      party_id: partyId,
      rules_contract_id: "00rules",
      action: { type: "governance_cancel_action", proposal_cid: "00stranded" },
      governance_type: "core_self",
    });
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("does not offer a cancellation vote while the proposer is authorized", () => {
    renderFeed([proposal({ proposer_not_authorized: false })]);

    expect(screen.queryByRole("button", { name: "Propose cancellation" })).toBeNull();
  });

  it("leaves a stranded proposer to retract its own proposal", () => {
    renderFeed([proposal({ proposer: memberId })]);

    expect(screen.getByRole("button", { name: "Cancel proposal" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Propose cancellation" })).toBeNull();
  });

  it("points at the open vote instead of offering a second one", () => {
    renderFeed([proposal()], [cancellation("00stranded")]);

    const cards = screen.getAllByRole("button", { name: /Review/ });
    expect(screen.queryByRole("button", { name: "Propose cancellation" })).toBeNull();
    cards.forEach((review) => fireEvent.click(review));
    expect(screen.getByText(/Members are voting to cancel it\./)).toBeTruthy();
  });
});

describe("a cancellation vote card", () => {
  it("names the proposal it cancels and shows its description", () => {
    renderFeed([proposal()], [cancellation("00stranded")]);

    expect(screen.getByText("Cancel Action")).toBeTruthy();
    // The title carries the cancelled proposal's label beside the action.
    expect(screen.getAllByText("Transfer").length).toBeGreaterThanOrEqual(2);
    expect(screen.getByText(`agent::1220cc…cccccc`)).toBeTruthy();

    screen.getAllByRole("button", { name: /Review/ }).forEach((review) => fireEvent.click(review));
    expect(screen.getByText("Description of the proposal this vote cancels")).toBeTruthy();
  });

  it("falls back to the contract id when the proposal is not loaded", () => {
    renderFeed([], [cancellation("00unloadedproposal")]);

    expect(screen.getByText("Proposal not loaded")).toBeTruthy();
    expect(screen.getByText("00unloadedpr…")).toBeTruthy();
  });
});
