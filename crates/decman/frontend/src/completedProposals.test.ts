import { describe, expect, it } from "vitest";

import {
  EMPTY_AUDIT_SNAPSHOT,
  completedProposals,
  nextAuditSnapshot,
  type PendingProposals,
} from "./completedProposals";
import type { ActionType, AuditLogEntry } from "./types";

const partyId = `shared::1220${"a".repeat(64)}`;
const otherParty = `other::1220${"c".repeat(64)}`;

let nextId = 1;

const row = (overrides: Partial<AuditLogEntry>): AuditLogEntry => {
  const id = nextId++;
  return {
    id,
    timestamp: 1_700_000_000 + id,
    event_type: "confirm",
    party_id: partyId,
    member_party_id: `member::1220${"b".repeat(64)}`,
    governance_type: "core_domain",
    action_summary: "governance_set_threshold",
    details: {},
    status: "success",
    created_at: 1_700_000_000 + id,
    ...overrides,
  };
};

const domain = (event_type: string, proposal_cid: string, more: Partial<AuditLogEntry> = {}) =>
  row({ event_type, details: { proposal_cid }, ...more });

const self = (event_type: string, action: unknown, more: Partial<AuditLogEntry> = {}) =>
  row({ event_type, governance_type: "core_self", details: { action }, ...more });

const loaded = (overrides: Partial<PendingProposals> = {}): PendingProposals => ({
  partyId,
  actions: [],
  domainActions: [],
  nextCursor: null,
  ...overrides,
});

const threshold3: ActionType = { type: "governance_set_threshold", new_threshold: 3 };

describe("completedProposals", () => {
  it("ignores failed rows: a failed execute did not execute anything", () => {
    const result = completedProposals(
      [domain("confirm", "00p"), domain("execute", "00p", { status: "failed" })],
      [loaded()],
    );

    expect(result.map((p) => p.outcome)).toEqual(["confirmed"]);
  });

  it("keeps a failed propose that created its proposal", () => {
    // The proposal was created; only this node's own confirmation failed.
    const createdButUnconfirmed = domain("propose", "00created", {
      status: "failed",
      action_summary: "transfer",
    });

    const result = completedProposals([createdButUnconfirmed], [loaded()]);

    expect(result).toHaveLength(1);
    expect(result[0]).toMatchObject({ outcome: "proposed", label: "Transfer" });
  });

  it("follows a failed propose with a cid to its later execute", () => {
    const result = completedProposals(
      [domain("propose", "00p", { status: "failed" }), domain("execute", "00p")],
      [loaded()],
    );

    expect(result.map((p) => p.outcome)).toEqual(["executed"]);
  });

  it("drops every other failed row", () => {
    const result = completedProposals(
      [
        // Creating the proposal failed, so there is no cid and no proposal.
        row({
          event_type: "propose",
          status: "failed",
          details: { proposal: { type: "transfer" } },
        }),
        domain("confirm", "00c", { status: "failed" }),
        domain("cancel_proposal", "00x", { status: "failed" }),
        self("confirm", { type: "governance_set_threshold", new_threshold: 3 }, {
          status: "failed",
        }),
      ],
      [loaded()],
    );

    expect(result).toEqual([]);
  });

  it("matches a pending self action whatever the key order of its JSON", () => {
    const reordered = { new_threshold: 3, type: "governance_set_threshold" };

    const result = completedProposals(
      [self("confirm", reordered)],
      [loaded({ actions: [{ action: threshold3 }] })],
    );

    expect(result).toEqual([]);
  });

  it("treats the same self action proposed again after an execute as a new proposal", () => {
    const rows = [
      self("confirm", threshold3),
      self("execute", threshold3),
      self("confirm", threshold3),
    ];

    expect(completedProposals(rows, [loaded()]).map((p) => p.outcome)).toEqual([
      "executed",
      "confirmed",
    ]);
    // Once it is pending again, nothing about it belongs under Completed.
    expect(
      completedProposals(rows, [loaded({ actions: [{ action: threshold3 }] })]),
    ).toEqual([]);
  });

  it("keeps rows that name only a confirmation out", () => {
    const result = completedProposals(
      [
        row({ event_type: "expire", details: { confirmation_cid: "00c1" } }),
        row({ event_type: "cancel", details: { confirmation_cid: "00c2" } }),
      ],
      [loaded()],
    );

    expect(result).toEqual([]);
  });

  it("shows an execute even when the party's pending feed is unknown", () => {
    const result = completedProposals([domain("execute", "00p", { party_id: otherParty })], []);

    expect(result).toHaveLength(1);
    expect(result[0]).toMatchObject({ partyId: otherParty, outcome: "executed" });
  });

  it("trusts a stale pending feed only for executed and cancelled proposals", () => {
    const rows = [
      domain("confirm", "00open"),
      domain("execute", "00done"),
      domain("cancel_proposal", "00gone"),
    ];

    // The feed stops refreshing after a load-more. A proposal it no longer
    // lists may have closed since, or may never have been reloaded.
    const result = completedProposals(rows, [loaded()], false);

    expect(result.map((p) => p.outcome)).toEqual(["executed", "cancelled"]);
    expect(completedProposals(rows, [loaded()]).map((p) => p.outcome)).toEqual([
      "confirmed",
      "executed",
      "cancelled",
    ]);
  });

  it("skips a propose row logged before the row carried its proposal cid", () => {
    const legacy = row({
      event_type: "propose",
      action_summary: "transfer",
      details: { party_id: partyId, proposal: { type: "transfer" } },
    });

    expect(completedProposals([legacy], [loaded()])).toEqual([]);
  });

  it("names a domain proposal after its propose row, else after its cid", () => {
    const result = completedProposals(
      [
        domain("propose", "00named", { action_summary: "accept_transfer" }),
        domain("execute", "00named"),
        domain("execute", "00abcdef123456"),
      ],
      [loaded()],
    );

    expect(result.map((p) => p.label).sort()).toEqual([
      "Accept transfer",
      "Proposal 00abcdef…",
    ]);
  });

  it("times an entry by its newest row, in whatever order the rows arrive", () => {
    const propose = domain("propose", "00p");
    const execute = domain("execute", "00p");

    const [entry] = completedProposals([execute, propose], [loaded()]);

    expect(entry.ts).toBe(execute.timestamp);
    expect(entry.outcome).toBe("executed");
  });
});

describe("completedProposals with snapshot timing", () => {
  const seenAt = (rows: AuditLogEntry[], at: number) =>
    new Map(rows.map((r) => [r.id, at]));

  it("guesses 'no longer pending' only from a pending feed requested after the row was seen", () => {
    // Right after a propose the audit poll can show the new row while the
    // committed pending feed was requested before the proposal existed.
    const rows = [domain("propose", "00new")];
    const firstSeen = seenAt(rows, 1_000);

    const before = completedProposals(rows, [loaded()], true, {
      firstSeen,
      pendingRequestedAt: 900,
    });
    const same = completedProposals(rows, [loaded()], true, {
      firstSeen,
      pendingRequestedAt: 1_000,
    });
    const after = completedProposals(rows, [loaded()], true, {
      firstSeen,
      pendingRequestedAt: 1_100,
    });

    expect(before).toEqual([]);
    expect(same).toEqual([]);
    expect(after.map((p) => p.outcome)).toEqual(["proposed"]);
  });

  it("times the guess by the newest row of the proposal", () => {
    const propose = domain("propose", "00p");
    const confirm = domain("confirm", "00p");
    const firstSeen = new Map([
      [propose.id, 100],
      [confirm.id, 2_000],
    ]);

    const result = completedProposals([propose, confirm], [loaded()], true, {
      firstSeen,
      pendingRequestedAt: 1_000,
    });

    expect(result).toEqual([]);
  });

  it("does not guess for a row it has no first-seen time for", () => {
    const rows = [domain("confirm", "00p")];

    const result = completedProposals(rows, [loaded()], true, {
      firstSeen: new Map(),
      pendingRequestedAt: 1_000,
    });

    expect(result).toEqual([]);
  });

  it("shows executed and cancelled proposals whatever the timing", () => {
    const rows = [domain("execute", "00a"), domain("cancel_proposal", "00b")];

    const result = completedProposals(rows, [loaded()], true, {
      firstSeen: seenAt(rows, 1_000),
      pendingRequestedAt: 0,
    });

    expect(result.map((p) => p.outcome)).toEqual(["executed", "cancelled"]);
  });
});

describe("nextAuditSnapshot", () => {
  it("keeps when a row was first seen and forgets rows that left the window", () => {
    const old = domain("confirm", "00old");
    const kept = domain("confirm", "00kept");
    const fresh = domain("execute", "00kept");
    const first = nextAuditSnapshot(EMPTY_AUDIT_SNAPSHOT, [kept, old], 100);

    const second = nextAuditSnapshot(first, [fresh, kept], 250);

    expect(second.entries).toEqual([fresh, kept]);
    expect([...second.firstSeen]).toEqual([
      [fresh.id, 250],
      [kept.id, 100],
    ]);
  });
});
