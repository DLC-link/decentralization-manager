import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { SnackbarProvider } from "../contexts";
import type { GovernanceAction, PendingInvitation, WorkflowRun } from "../types";
import { NotificationsView, type PartyActions } from "./NotificationsView";

const partyId = `shared::1220${"a".repeat(64)}`;
const memberId = `member::1220${"b".repeat(64)}`;
const kickedId = `kicked::1220${"c".repeat(64)}`;

const renderFeed = ({
  pendingInvitations = [],
  partyActions = [],
  workflowRuns = [],
}: {
  pendingInvitations?: PendingInvitation[];
  partyActions?: PartyActions[];
  workflowRuns?: WorkflowRun[];
}) =>
  render(
    <SnackbarProvider>
      <NotificationsView
        pendingInvitations={pendingInvitations}
        partyActions={partyActions}
        workflowRuns={workflowRuns}
        loading={false}
        onInvitationsChanged={() => {}}
        onActionsChanged={() => {}}
        onWorkflowsChanged={() => {}}
        onSelectParty={() => {}}
      />
    </SnackbarProvider>,
  );

// A line through a "1" makes it read as a "4" (#542).
const struckThrough = () =>
  Array.from(document.body.querySelectorAll("*")).filter((el) =>
    getComputedStyle(el).textDecoration.includes("line-through"),
  );

const openReview = () =>
  fireEvent.click(screen.getByRole("button", { name: /Review/ }));

/** The value cell beside the "Threshold" label on an invitation or run card. */
const thresholdRow = () => {
  const label = screen.getByText("Threshold");
  return label.nextElementSibling?.textContent ?? "";
};

describe("Threshold changes in the feed", () => {
  it("shows the old threshold on a Kick invitation without a strikethrough", () => {
    renderFeed({
      pendingInvitations: [
        {
          id: "kick-invite",
          invitation_type: "Kick",
          coordinator_pubkey: "c0ffee".repeat(8),
          coordinator_name: "Coordinator One",
          received_at: 1_700_000_000,
          participants: [],
          dar_filenames: [],
          dar_hashes: [],
          package_names: [],
          contract_intents: [],
          workflow_instance: "kick-1",
          dec_party_id: partyId,
          kicked_participant: kickedId,
          previous_threshold: 1,
          new_threshold: 2,
        },
      ],
    });

    openReview();

    expect(thresholdRow()).toBe("1 → 2");
    expect(struckThrough()).toEqual([]);
  });

  it("shows the confirmation threshold a change-threshold invitation asks for", () => {
    renderFeed({
      pendingInvitations: [
        {
          id: "change-threshold-invite",
          invitation_type: "ChangeThreshold",
          coordinator_pubkey: "c0ffee".repeat(8),
          received_at: 1_700_000_000,
          participants: [],
          dar_filenames: [],
          dar_hashes: [],
          package_names: [],
          contract_intents: [],
          dec_party_id: partyId,
          previous_threshold: 2,
          new_threshold: 1,
          confirmation_threshold: 3,
        },
      ],
    });

    openReview();

    expect(thresholdRow()).toBe("2 → 1");
    expect(screen.getByText("Confirmation").nextElementSibling?.textContent).toBe(
      "3",
    );
  });

  it("shows the old threshold on a Kick run without a strikethrough", () => {
    renderFeed({
      workflowRuns: [
        {
          instance_name: "kick-1",
          kind: "Kick",
          role: "Coordinator",
          status: "inprogress",
          current_step: "WaitingForPeers",
          step_index: 1,
          step_total: 4,
          config_json: "{}",
          expected_peers: [],
          completed_peers: [],
          connected_peers: [],
          participants: [],
          package_names: [],
          dar_filenames: [],
          dismissed: false,
          created_at: 1_700_000_000,
          updated_at: 1_700_000_000,
          kicked_participant: kickedId,
          previous_threshold: 1,
          new_threshold: 2,
        },
      ],
    });

    openReview();

    expect(thresholdRow()).toBe("1 → 2");
    expect(struckThrough()).toEqual([]);
  });

  it("shows the old threshold in what you're signing without a strikethrough", () => {
    const action: GovernanceAction = {
      action_hash: "hash-of-set-threshold",
      action: { type: "governance_set_threshold", new_threshold: 2 },
      confirmations: [],
      confirmation_count: 0,
      executable_confirmation_cids: [],
      can_execute: false,
      last_confirmation_at: 0,
    };
    renderFeed({
      partyActions: [
        {
          partyId,
          rulesContractId: "00rules",
          memberPartyId: memberId,
          governanceType: "core_self",
          threshold: 1,
          actions: [action],
          domainActions: [],
        },
      ],
    });

    openReview();

    expect(screen.getByText("Exactly what you're signing")).toBeTruthy();
    expect(screen.getAllByText("1 → 2").length).toBeGreaterThan(0);
    expect(struckThrough()).toEqual([]);
  });
});
