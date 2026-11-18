import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { SnackbarProvider } from "../contexts";
import type { PendingInvitation } from "../types";
import { NotificationsView } from "./NotificationsView";

const partyId = `shared::1220${"a".repeat(64)}`;
const memberA = `member-a::1220${"b".repeat(64)}`;
const memberB = `member-b::1220${"c".repeat(64)}`;
const rotatedKey = `1220${"d".repeat(64)}`;

const invitation: PendingInvitation = {
  id: "rotate-key-invite",
  invitation_type: "RotateKey",
  coordinator_pubkey: "c0ffee".repeat(8),
  received_at: 1_700_000_000,
  participants: [memberA, memberB],
  dar_filenames: [],
  dar_hashes: [],
  package_names: [],
  contract_intents: [],
  workflow_instance: "shared-rotate-key-1",
  dec_party_id: partyId,
  rotated_key: rotatedKey,
  new_threshold: 2,
  confirmation_threshold: 2,
};

const renderFeed = () =>
  render(
    <SnackbarProvider>
      <NotificationsView
        pendingInvitations={[invitation]}
        partyActions={[]}
        workflowRuns={[]}
        loading={false}
        onInvitationsChanged={() => {}}
        onActionsChanged={() => {}}
        onWorkflowsChanged={() => {}}
        onSelectParty={() => {}}
      />
    </SnackbarProvider>,
  );

describe("A rotate-key invitation in the feed", () => {
  it("names the key it replaces in the title", () => {
    renderFeed();

    expect(screen.getByText(/Replace Daml key 1220dddddd…dddddddd/)).toBeTruthy();
  });

  it("shows the party, the members, and the replaced key", () => {
    renderFeed();
    fireEvent.click(screen.getByRole("button", { name: /Review/ }));

    expect(screen.getByText("Dec party")).toBeTruthy();
    expect(screen.getByText("Participants (2)")).toBeTruthy();
    expect(screen.getByTestId("rotated-key").textContent).toContain(
      "1220dddddd…dddddddd",
    );
  });

  it("says the replaced key is the coordinator's and the threshold stays", () => {
    renderFeed();
    fireEvent.click(screen.getByRole("button", { name: /Review/ }));

    expect(screen.getByTestId("rotated-key-owner").textContent).toContain(
      "the coordinator's own Daml key",
    );
    expect(screen.getByTestId("invitation-threshold").textContent).toBe("2, unchanged");
  });
});
