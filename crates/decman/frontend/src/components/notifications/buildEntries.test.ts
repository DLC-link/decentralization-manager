import { describe, expect, it } from "vitest";

import type {
  AuditLogEntry,
  DomainGovernanceAction,
  GovernanceAction,
  HandledInvitation,
  PendingInvitation,
  WorkflowRun,
} from "../../types";
import { buildEntries, filterEntries, partyEntryCounts, type Entry } from "./buildEntries";
import type { PartyActions } from "./types";

const ns = "1220" + "a".repeat(64);
const alpha = `alpha::${ns}`;
const beta = `beta::${ns}`;
const me = `member::${ns}`;
const other = `other::${ns}`;

const confirmation = (party: string, created_at = 0) => ({
  contract_id: `c-${party}`,
  confirming_party: party,
  created_at,
  expires_at: 0,
});

const action = (
  hash: string,
  confirmations: ReturnType<typeof confirmation>[],
  can_execute = false,
  last_confirmation_at = 0,
): GovernanceAction => ({
  action_hash: hash,
  action: { type: "governance_set_threshold", new_threshold: 2 },
  confirmations: confirmations.map((c) => ({
    ...c,
    action: { type: "governance_set_threshold", new_threshold: 2 },
  })),
  confirmation_count: confirmations.length,
  executable_confirmation_cids: [],
  can_execute,
  last_confirmation_at,
});

const domain = (
  cid: string,
  confirmations: ReturnType<typeof confirmation>[],
  extra: Partial<DomainGovernanceAction> = {},
): DomainGovernanceAction => ({
  proposal_cid: cid,
  action_label: "Transfer",
  confirmations,
  confirmation_count: confirmations.length,
  executable_confirmation_cids: [],
  can_execute: false,
  orphaned: false,
  proposer: me,
  proposer_not_authorized: false,
  created_at: 100,
  ...extra,
});

const party = (
  partyId: string,
  actions: GovernanceAction[],
  domainActions: DomainGovernanceAction[] = [],
): PartyActions => ({
  partyId,
  memberPartyId: me,
  governanceType: "core_self",
  threshold: 2,
  actions,
  domainActions,
});

const invitation = (id: string, received_at: number): PendingInvitation =>
  ({ id, invitation_type: "Onboarding", received_at }) as PendingInvitation;

const run = (instance_name: string, status: WorkflowRun["status"], updated_at: number) =>
  ({ instance_name, status, updated_at }) as WorkflowRun;

const handled = (id: string, handled_at: number): HandledInvitation =>
  ({ invitation: invitation(id, 0), outcome: "accepted", handled_at }) as HandledInvitation;

const feeds = (overrides: Partial<Parameters<typeof buildEntries>[0]> = {}) => ({
  pendingInvitations: [] as PendingInvitation[],
  partyActions: [] as PartyActions[],
  workflowRuns: [] as WorkflowRun[],
  governanceAudit: [] as AuditLogEntry[],
  handledInvitations: [] as HandledInvitation[],
  pendingLive: true,
  ...overrides,
});

const summary = (entries: Entry[]) =>
  entries.map((e) => `${e.key} ${e.group} ${e.kind} ${e.partyId ?? "-"}`);

describe("buildEntries", () => {
  it("puts each feed item in the group of what it needs from the operator", () => {
    const entries = buildEntries(
      feeds({
        pendingInvitations: [invitation("inv-1", 50)],
        partyActions: [
          party(
            alpha,
            [
              // Executable: yours to finish.
              action("exec", [confirmation(me)], true, 40),
              // Confirmed by you, waiting on the others.
              action("wait", [confirmation(me)], false, 30),
              // Not confirmed by you yet.
              action("new", [confirmation(other)], false, 20),
            ],
            [
              domain("d-mine", [confirmation(other)], { created_at: 15 }),
              domain("d-wait", [confirmation(me)], { created_at: 14 }),
              // Orphaned proposals can only be expired: still yours.
              domain("d-orphan", [confirmation(me, 13)], { orphaned: true, created_at: undefined }),
            ],
          ),
        ],
        workflowRuns: [run("live", "inprogress", 12), run("done", "failed", 11)],
        handledInvitations: [handled("inv-old", 10)],
      }),
    );

    expect(summary(entries)).toEqual([
      "inv-inv-1 you inv -",
      `act-${alpha}-exec you gov ${alpha}`,
      `act-${alpha}-wait prog gov ${alpha}`,
      `act-${alpha}-new you gov ${alpha}`,
      `dom-${alpha}-d-mine you gov ${alpha}`,
      `dom-${alpha}-d-wait prog gov ${alpha}`,
      `dom-${alpha}-d-orphan you gov ${alpha}`,
      "wf-live prog wf -",
      "wf-done done wf -",
      "inv-done-inv-old done inv -",
    ]);
  });

  it("dates an orphaned proposal by its newest confirmation", () => {
    const [entry] = buildEntries(
      feeds({
        partyActions: [
          party(alpha, [], [
            domain("d", [confirmation(me, 7), confirmation(other, 9)], {
              orphaned: true,
              created_at: undefined,
            }),
          ]),
        ],
      }),
    );
    expect(entry.ts).toBe(9);
  });

  it("drops a handled invitation that is pending again", () => {
    const entries = buildEntries(
      feeds({
        pendingInvitations: [invitation("again", 5)],
        handledInvitations: [handled("again", 4), handled("gone", 3)],
      }),
    );
    expect(entries.map((e) => e.key)).toEqual(["inv-again", "inv-done-gone"]);
  });

  it("keeps the item each card needs", () => {
    const pending = invitation("inv-1", 1);
    const [entry] = buildEntries(feeds({ pendingInvitations: [pending] }));
    expect(entry.item).toEqual({ type: "invitation", invitation: pending });
  });
});

describe("filterEntries", () => {
  const entries = buildEntries(
    feeds({
      pendingInvitations: [invitation("inv-1", 9)],
      partyActions: [
        party(alpha, [action("a1", [], false, 8), action("a2", [confirmation(me)], false, 7)]),
        party(beta, [action("b1", [], false, 6)]),
      ],
      workflowRuns: [run("live", "inprogress", 5), run("done", "completed", 4)],
    }),
  );

  it("counts every group with no filter", () => {
    const { groupCount, grouped, anyVisible } = filterEntries(entries, [], "all", "all");
    expect(groupCount).toEqual({ all: 6, you: 3, prog: 2, done: 1 });
    expect(grouped.you.map((e) => e.key)).toEqual([
      "inv-inv-1",
      `act-${alpha}-a1`,
      `act-${beta}-b1`,
    ]);
    expect(anyVisible).toBe(true);
  });

  it("counts the status chips under the type filter, not the status one", () => {
    const { groupCount, grouped } = filterEntries(entries, [], "you", "gov");
    expect(groupCount).toEqual({ all: 3, you: 2, prog: 1, done: 0 });
    expect(grouped.you.map((e) => e.key)).toEqual([`act-${alpha}-a1`, `act-${beta}-b1`]);
    expect(grouped.prog).toEqual([]);
  });

  it("keeps entries without a party under a party filter", () => {
    const { groupCount, grouped } = filterEntries(entries, [beta], "all", "all");
    expect(groupCount).toEqual({ all: 4, you: 2, prog: 1, done: 1 });
    expect(grouped.you.map((e) => e.key)).toEqual(["inv-inv-1", `act-${beta}-b1`]);
  });

  it("reports when nothing matches", () => {
    const { anyVisible } = filterEntries(entries, [], "done", "inv");
    expect(anyVisible).toBe(false);
  });
});

describe("partyEntryCounts", () => {
  it("counts each party's entries, most first, from the unfiltered set", () => {
    const entries = buildEntries(
      feeds({
        pendingInvitations: [invitation("inv-1", 9)],
        partyActions: [
          party(alpha, [action("a1", [])]),
          party(beta, [action("b1", []), action("b2", [])], [domain("d", [])]),
        ],
      }),
    );
    expect(partyEntryCounts(entries)).toEqual([
      [beta, 3],
      [alpha, 1],
    ]);
  });
});
