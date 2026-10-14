import { fireEvent, render, screen, within } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { SnackbarProvider } from "../contexts";
import type {
  AuditLogEntry,
  DomainGovernanceAction,
  HandledInvitation,
  PendingInvitation,
  WorkflowRun,
} from "../types";
import { NotificationsView, type PartyActions } from "./NotificationsView";

const partyId = `shared::1220${"a".repeat(64)}`;
const memberId = `member::1220${"b".repeat(64)}`;

let nextId = 1;

/** One row of this node's governance audit log. */
const auditRow = (overrides: Partial<AuditLogEntry>): AuditLogEntry => {
  const id = nextId++;
  return {
    id,
    timestamp: 1_700_000_000 + id,
    event_type: "confirm",
    party_id: partyId,
    member_party_id: memberId,
    governance_type: "core_domain",
    action_summary: "governance_set_threshold",
    details: {},
    status: "success",
    created_at: 1_700_000_000 + id,
    ...overrides,
  };
};

/** The rows `/governance/propose` and the domain card buttons leave behind. */
const domainRow = (
  event_type: string,
  proposal_cid: string,
  overrides: Partial<AuditLogEntry> = {},
) =>
  auditRow({
    event_type,
    action_summary: event_type === "propose" ? "transfer" : "governance_set_threshold",
    details: { party_id: partyId, proposal_cid },
    ...overrides,
  });

/** A party whose pending feed is loaded in full. */
const loadedParty = (
  domainActions: DomainGovernanceAction[] = [],
  overrides: Partial<PartyActions> = {},
): PartyActions => ({
  partyId,
  rulesContractId: "00rules",
  memberPartyId: memberId,
  governanceType: "core_self",
  threshold: 2,
  actions: [],
  domainActions,
  nextCursor: null,
  ...overrides,
});

const pendingProposal = (proposal_cid: string): DomainGovernanceAction => ({
  proposal_cid,
  action_label: "Transfer",
  confirmations: [
    { contract_id: "00conf", confirming_party: memberId, created_at: 1, expires_at: 0 },
  ],
  confirmation_count: 1,
  executable_confirmation_cids: [],
  can_execute: false,
  orphaned: false,
  proposer: memberId,
  proposer_not_authorized: false,
  created_at: 1_700_000_000,
});

const renderFeed = ({
  partyActions = [loadedParty()],
  governanceAudit = [],
  workflowRuns = [],
  pendingInvitations = [],
  handledInvitations = [],
}: {
  partyActions?: PartyActions[];
  governanceAudit?: AuditLogEntry[];
  workflowRuns?: WorkflowRun[];
  pendingInvitations?: PendingInvitation[];
  handledInvitations?: HandledInvitation[];
}) =>
  render(
    <SnackbarProvider>
      <NotificationsView
        pendingInvitations={pendingInvitations}
        partyActions={partyActions}
        workflowRuns={workflowRuns}
        governanceAudit={governanceAudit}
        handledInvitations={handledInvitations}
        loading={false}
        onInvitationsChanged={() => {}}
        onActionsChanged={() => {}}
        onWorkflowsChanged={() => {}}
        onSelectParty={() => {}}
      />
    </SnackbarProvider>,
  );

/** The count a filter chip shows next to its label. */
const chipCount = (label: string): string => {
  const chip = screen.getByRole("button", { name: new RegExp(`^${label}`) });
  return chip.textContent?.replace(label, "") ?? "";
};

const showCompleted = () =>
  fireEvent.click(screen.getByRole("button", { name: /^Completed/ }));

const completedRows = () => screen.queryAllByTestId("completed-proposal");

describe("Completed governance proposals", () => {
  it("lists an executed proposal with its outcome", () => {
    renderFeed({
      governanceAudit: [
        domainRow("propose", "00executed"),
        domainRow("execute", "00executed"),
      ],
    });

    expect(chipCount("Completed")).toBe("1");
    showCompleted();

    const [row] = completedRows();
    expect(row.getAttribute("data-outcome")).toBe("executed");
    expect(within(row).getByText("Transfer")).toBeTruthy();
    expect(within(row).getByText("Executed")).toBeTruthy();
    expect(within(row).getByText(/^shared::/)).toBeTruthy();
  });

  it("lists a retracted proposal as cancelled", () => {
    renderFeed({
      governanceAudit: [
        domainRow("propose", "00retracted"),
        domainRow("cancel_proposal", "00retracted"),
      ],
    });

    showCompleted();

    const [row] = completedRows();
    expect(within(row).getByText("Cancelled")).toBeTruthy();
  });

  it("keeps a proposal that is still pending out of Completed", () => {
    renderFeed({
      partyActions: [loadedParty([pendingProposal("00pending")])],
      governanceAudit: [domainRow("confirm", "00pending")],
    });

    expect(chipCount("Completed")).toBe("0");
    expect(chipCount("In progress")).toBe("1");
    expect(completedRows()).toHaveLength(0);
  });

  it("says this node confirmed a proposal that is no longer pending", () => {
    renderFeed({
      governanceAudit: [domainRow("confirm", "00elsewhere")],
    });

    showCompleted();

    const [row] = completedRows();
    expect(row.getAttribute("data-outcome")).toBe("confirmed");
    expect(within(row).getByText("You confirmed")).toBeTruthy();
    expect(within(row).getByText(/no longer pending/)).toBeTruthy();
  });

  it("does not call a proposal finished while its party's feed is partly loaded", () => {
    renderFeed({
      partyActions: [loadedParty([], { nextCursor: "more" })],
      governanceAudit: [
        domainRow("confirm", "00beyondthefirstbatch"),
        domainRow("execute", "00executed"),
      ],
    });

    showCompleted();

    // The execute row is an outcome by itself; the confirm row is not.
    const rows = completedRows();
    expect(rows).toHaveLength(1);
    expect(rows[0].getAttribute("data-outcome")).toBe("executed");
  });

  it("does not call a proposal finished when its party's feed did not load", () => {
    renderFeed({
      partyActions: [],
      governanceAudit: [domainRow("confirm", "00unknown")],
    });

    expect(screen.getByText("No pending notifications.")).toBeTruthy();
    expect(completedRows()).toHaveLength(0);
  });

  it("folds every row of one proposal into one entry", () => {
    const threshold = { type: "governance_set_threshold", new_threshold: 3 };
    renderFeed({
      governanceAudit: [
        domainRow("propose", "00one"),
        domainRow("confirm", "00one"),
        domainRow("execute", "00one"),
        auditRow({
          governance_type: "core_self",
          event_type: "confirm",
          details: { party_id: partyId, action: threshold },
        }),
        auditRow({
          governance_type: "core_self",
          event_type: "execute",
          details: { party_id: partyId, action: threshold },
        }),
      ],
    });

    expect(chipCount("Completed")).toBe("2");
    showCompleted();

    const labels = completedRows().map((r) => r.textContent ?? "");
    expect(labels).toHaveLength(2);
    expect(labels.some((t) => t.startsWith("Set Threshold"))).toBe(true);
    expect(labels.some((t) => t.startsWith("Transfer"))).toBe(true);
  });

  it("shows only the audit entries under Completed with the Governance chip", () => {
    const finishedRun: WorkflowRun = {
      instance_name: "kick-1",
      kind: "Kick",
      role: "Coordinator",
      status: "completed",
      current_step: "Done",
      step_index: 4,
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
    };
    renderFeed({
      workflowRuns: [finishedRun],
      governanceAudit: [
        domainRow("execute", "00a"),
        domainRow("cancel_proposal", "00b"),
      ],
    });

    expect(chipCount("Completed")).toBe("3");
    fireEvent.click(screen.getByRole("button", { name: "Governance" }));
    expect(chipCount("Completed")).toBe("2");

    showCompleted();

    expect(completedRows()).toHaveLength(2);
    expect(screen.queryByTestId("workflow-run-card")).toBeNull();
  });

  it("pages the audit entries with the rest of Completed", () => {
    const governanceAudit = Array.from({ length: 30 }, (_, i) =>
      domainRow("execute", `00proposal${i}`),
    );
    renderFeed({ governanceAudit });

    expect(chipCount("Completed")).toBe("30");
    showCompleted();

    expect(completedRows()).toHaveLength(25);
    expect(screen.getByText("1–25 of 30")).toBeTruthy();
  });
});

const invitation = (
  id: string,
  overrides: Partial<PendingInvitation> = {},
): PendingInvitation => ({
  id,
  invitation_type: "Onboarding",
  coordinator_pubkey: "c0ffee".repeat(8),
  coordinator_name: "Coordinator One",
  received_at: 1_700_000_000,
  prefix: `party-${id}`,
  participants: [],
  dar_filenames: [],
  dar_hashes: [],
  package_names: [],
  contract_intents: [],
  workflow_instance: `run-${id}`,
  ...overrides,
});

const handled = (
  id: string,
  outcome: HandledInvitation["outcome"],
  overrides: Partial<PendingInvitation> = {},
): HandledInvitation => ({
  invitation: invitation(id, overrides),
  outcome,
  handled_at: 1_700_000_100,
});

const handledRows = () => screen.queryAllByTestId("handled-invitation");

describe("Completed invitations", () => {
  it("lists an accepted invitation with its outcome", () => {
    renderFeed({ handledInvitations: [handled("a", "accepted")] });

    expect(chipCount("Completed")).toBe("1");
    showCompleted();

    const [row] = handledRows();
    expect(row.getAttribute("data-outcome")).toBe("accepted");
    expect(within(row).getByText("Onboarding invitation")).toBeTruthy();
    expect(within(row).getByText("party-a")).toBeTruthy();
    expect(within(row).getByText("Accepted")).toBeTruthy();
  });

  it("lists a declined invitation, naming its party or else its sender", () => {
    renderFeed({
      handledInvitations: [
        handled("kick", "declined", {
          invitation_type: "Kick",
          prefix: undefined,
          dec_party_id: partyId,
        }),
        handled("dars", "declined", { invitation_type: "Dars", prefix: undefined }),
      ],
    });

    showCompleted();

    const rows = handledRows();
    expect(rows).toHaveLength(2);
    for (const row of rows) {
      expect(within(row).getByText("Declined")).toBeTruthy();
    }
    expect(within(rows[0]).getByText(/^shared::/)).toBeTruthy();
    expect(within(rows[1]).getByText("from Coordinator One")).toBeTruthy();
  });

  it("fills Completed with the Invitations chip", () => {
    renderFeed({
      governanceAudit: [domainRow("execute", "00done")],
      pendingInvitations: [invitation("waiting")],
      handledInvitations: [handled("a", "accepted"), handled("b", "declined")],
    });

    fireEvent.click(screen.getByRole("button", { name: "Invitations" }));

    expect(chipCount("Completed")).toBe("2");
    expect(chipCount("Needs you")).toBe("1");
    showCompleted();
    expect(handledRows()).toHaveLength(2);
    expect(completedRows()).toHaveLength(0);
    expect(screen.queryByTestId("invitation-card")).toBeNull();
  });

  it("leaves out an answered invitation the coordinator has sent again", () => {
    renderFeed({
      pendingInvitations: [invitation("a")],
      handledInvitations: [handled("a", "declined"), handled("b", "accepted")],
    });

    expect(chipCount("Completed")).toBe("1");
    showCompleted();
    const rows = handledRows();
    expect(rows).toHaveLength(1);
    expect(rows[0].getAttribute("data-outcome")).toBe("accepted");
  });
});
