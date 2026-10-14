import { describe, expect, it } from "vitest";

import { completedProposals, type PendingProposals } from "./completedProposals";
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
