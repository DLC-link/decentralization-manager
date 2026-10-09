import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { SnackbarProvider } from "../contexts";
import type { DomainGovernanceAction } from "../types";
import { NotificationsView } from "./NotificationsView";

const partyId = `shared::1220${"a".repeat(64)}`;
const memberId = `member::1220${"b".repeat(64)}`;
const formerAgentId = `agent::1220${"c".repeat(64)}`;

const proposal = (
  overrides: Partial<DomainGovernanceAction>,
): DomainGovernanceAction => ({
  proposal_cid: "00proposal",
  action_label: "Transfer",
  confirmations: [],
  confirmation_count: 0,
  executable_confirmation_cids: [],
  can_execute: false,
  orphaned: false,
  proposer: formerAgentId,
  proposer_not_authorized: false,
  created_at: 1_700_000_000,
  ...overrides,
});

const renderFeed = (domainAction: DomainGovernanceAction) =>
  render(
    <SnackbarProvider>
      <NotificationsView
        pendingInvitations={[]}
        partyActions={[
          {
            partyId,
            rulesContractId: "00rules",
            memberPartyId: memberId,
            governanceType: "core_domain",
            threshold: 2,
            actions: [],
            domainActions: [domainAction],
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

describe("DomainActionCard proposer authorization", () => {
  it("offers Confirm while the rules authorize the proposer", () => {
    renderFeed(proposal({ proposer_not_authorized: false }));

    expect(screen.getByRole("button", { name: "Confirm" })).toBeTruthy();
    expect(screen.queryByText("Proposer not authorized")).toBeNull();
  });

  it("withholds Confirm the ledger would reject, and says why", () => {
    renderFeed(proposal({ proposer_not_authorized: true }));

    expect(screen.getByText("Proposer not authorized")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Confirm" })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: /Review/ }));
    expect(
      screen.getByText(/the ledger rejects every new confirmation/),
    ).toBeTruthy();
  });

  it("leaves the proposer able to cancel its own stranded proposal", () => {
    renderFeed(
      proposal({ proposer: memberId, proposer_not_authorized: true }),
    );

    expect(screen.getByRole("button", { name: "Cancel proposal" })).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Confirm" })).toBeNull();
  });
});
