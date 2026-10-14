import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

import { SnackbarProvider } from "../contexts";
import type { DomainGovernanceAction, GovernanceAction } from "../types";
import { NotificationsView, type PartyActions } from "./NotificationsView";

const partyId = `shared::1220${"a".repeat(64)}`;
const memberId = `member::1220${"b".repeat(64)}`;
const receiverId = `bob::1220${"e".repeat(64)}`;
// Longer than the 16 characters the summary row shows, so only the payload
// block can carry it whole.
const proposalCid = `00${"f".repeat(66)}`;
const templateId =
  "4d1f:Governance.TokenCustody.TransferProposal:TransferProposal";

const createArguments = {
  governanceParty: partyId,
  proposer: memberId,
  transfer: {
    receiver: receiverId,
    amount: "12.5000000000",
    meta: { values: { memo: "March rent" } },
  },
};

const domainAction = (
  overrides: Partial<DomainGovernanceAction>,
): DomainGovernanceAction => ({
  proposal_cid: proposalCid,
  action_label: "Transfer",
  description: "Pay March rent",
  confirmations: [],
  confirmation_count: 0,
  executable_confirmation_cids: [],
  can_execute: false,
  orphaned: false,
  proposer: memberId,
  proposer_not_authorized: false,
  created_at: 1_700_000_000,
  ...overrides,
});

const renderFeed = (party: Partial<PartyActions>) =>
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
            domainActions: [],
            ...party,
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

const openReview = () =>
  fireEvent.click(screen.getByRole("button", { name: /Review/ }));

const payloadBlock = () =>
  screen.getByRole("region", { name: "Full payload JSON" });

describe("Review on a domain proposal", () => {
  afterEach(() => {
    Reflect.deleteProperty(navigator, "clipboard");
  });

  it("shows the full create arguments with the contract they belong to", () => {
    renderFeed({
      domainActions: [
        domainAction({
          proposal_template_id: templateId,
          proposal_payload: createArguments,
        }),
      ],
    });

    openReview();

    const json = payloadBlock().textContent ?? "";
    expect(json).toContain('"memo": "March rent"');
    expect(json).toContain('"amount": "12.5000000000"');
    expect(json).toContain(`"receiver": "${receiverId}"`);
    expect(json).toContain(`"contract_id": "${proposalCid}"`);
    expect(json).toContain(`"template_id": "${templateId}"`);
    // The summary stays above the JSON.
    expect(screen.getByText("Pay March rent")).toBeTruthy();
  });

  it("copies exactly the JSON it shows", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    // jsdom has no clipboard, and the clipboard is the browser boundary here.
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText },
      configurable: true,
    });
    renderFeed({
      domainActions: [
        domainAction({
          proposal_template_id: templateId,
          proposal_payload: createArguments,
        }),
      ],
    });

    openReview();
    fireEvent.click(screen.getByRole("button", { name: "Copy payload JSON" }));

    await waitFor(() =>
      expect(writeText).toHaveBeenCalledWith(payloadBlock().textContent),
    );
  });

  it("keeps the summary when the response carries no payload", () => {
    renderFeed({ domainActions: [domainAction({})] });

    openReview();

    expect(screen.getByText("Exactly what you're signing")).toBeTruthy();
    expect(screen.getByText("Pay March rent")).toBeTruthy();
    expect(
      screen.queryByRole("region", { name: "Full payload JSON" }),
    ).toBeNull();
  });
});

describe("Review on a self-management action", () => {
  it("shows the parsed action the confirm submits", () => {
    const action: GovernanceAction = {
      action_hash: "hash-of-set-threshold",
      action: { type: "governance_set_threshold", new_threshold: 3 },
      confirmations: [],
      confirmation_count: 0,
      executable_confirmation_cids: [],
      can_execute: false,
      last_confirmation_at: 0,
    };
    renderFeed({ governanceType: "core_self", actions: [action] });

    openReview();

    const json = payloadBlock().textContent ?? "";
    expect(JSON.parse(json)).toEqual(action.action);
  });
});
