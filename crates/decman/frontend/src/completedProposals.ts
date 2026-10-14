import { formatActionType } from "./governanceFormat";
import type { ActionType, AuditLogEntry } from "./types";

/**
 * How a proposal ended, as far as this node's own audit log can tell.
 *
 * The log holds only actions this node took. When another member executed a
 * proposal, or it lapsed, the newest row here is still this node's propose or
 * confirm, so the outcome names that action instead.
 */
export type ProposalOutcome = "executed" | "cancelled" | "proposed" | "confirmed";

export interface CompletedProposal {
  /** Unique per proposal, and stable across refreshes. */
  key: string;
  partyId: string;
  /** Epoch seconds of the row the outcome comes from. */
  ts: number;
  label: string;
  outcome: ProposalOutcome;
}

/** The part of a party's pending feed this module reads. */
export interface PendingProposals {
  partyId: string;
  actions: { action: ActionType }[];
  domainActions: { proposal_cid: string }[];
  /** Null once every domain proposal of the party is loaded. */
  nextCursor?: string | null;
}

/** Events that end a proposal. Every later row belongs to a new one. */
const TERMINAL_EVENTS = new Set(["execute", "cancel_proposal"]);

/** Domain events whose row carries the proposal's contract id. */
const DOMAIN_EVENTS = new Set(["propose", "confirm", "execute", "cancel_proposal"]);

/**
 * JSON with object keys sorted. The audit details pass through a key-sorted
 * map on the server while the pending feed keeps field order, so a plain
 * `JSON.stringify` of the same action would differ between the two.
 */
const canonicalJson = (value: unknown): string => {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  if (value !== null && typeof value === "object") {
    const fields = value as Record<string, unknown>;
    const body = Object.keys(fields)
      .filter((k) => fields[k] !== undefined)
      .sort()
      .map((k) => `${JSON.stringify(k)}:${canonicalJson(fields[k])}`)
      .join(",");
    return `{${body}}`;
  }
  return JSON.stringify(value) ?? "null";
};

const domainKey = (partyId: string, proposalCid: string) =>
  `${partyId}|dom|${proposalCid}`;
const selfKey = (partyId: string, action: unknown) =>
  `${partyId}|self|${canonicalJson(action)}`;

type Identity =
  | { key: string; kind: "domain"; proposalCid: string }
  | { key: string; kind: "self"; action: ActionType };

/**
 * Which proposal a row belongs to, or null when the row names none.
 *
 * Domain rows carry `proposal_cid`. Self-governance has no proposal contract:
 * its confirmations group by the action they carry, as the pending feed does.
 * Expire and cancel rows name only one confirmation, not the proposal, and
 * neither ends a proposal, so they are left out.
 */
const identify = (row: AuditLogEntry): Identity | null => {
  const details: unknown = row.details;
  if (details === null || typeof details !== "object") return null;
  const fields = details as Record<string, unknown>;
  if (row.governance_type === "core_domain") {
    if (!DOMAIN_EVENTS.has(row.event_type)) return null;
    const cid = fields.proposal_cid;
    if (typeof cid !== "string" || cid === "") return null;
    return { key: domainKey(row.party_id, cid), kind: "domain", proposalCid: cid };
  }
  if (row.governance_type === "core_self") {
    if (row.event_type !== "confirm" && row.event_type !== "execute") return null;
    const action = fields.action;
    if (action === null || typeof action !== "object") return null;
    return {
      key: selfKey(row.party_id, action),
      kind: "self",
      action: action as ActionType,
    };
  }
  return null;
};

/** `accept_transfer` → `Accept transfer`. */
const humanize = (tag: string): string => {
  const words = tag.replace(/_/g, " ");
  return words.charAt(0).toUpperCase() + words.slice(1);
};

const labelFor = (identity: Identity, rows: AuditLogEntry[]): string => {
  if (identity.kind === "self") {
    const known = formatActionType(identity.action);
    // Same title the pending card shows; a type with no display name falls
    // back to its tag.
    return known === identity.action.type
      ? humanize(known.replace(/^governance_/, ""))
      : known.replace(/Governance /i, "");
  }
  // A domain confirm or execute row carries a placeholder action, so only the
  // propose row knows what the proposal does.
  const propose = rows.find((r) => r.event_type === "propose");
  return propose
    ? humanize(propose.action_summary)
    : `Proposal ${identity.proposalCid.slice(0, 8)}…`;
};

/**
 * Turn this node's audit rows into one Completed entry per finished proposal.
 *
 * Only successful rows count. Rows group by proposal, and the newest row gives
 * the outcome. A proposal whose newest row is a propose or confirm is still
 * open as far as this node knows. It is shown only when the party's pending
 * feed is loaded in full and does not list it. Without that feed nobody can
 * tell "finished" from "not loaded", so such a proposal stays out.
 *
 * `pendingLive` is false while the pending feed is not being refreshed, as
 * after a load-more. A stale feed proves nothing about what is still open, so
 * only executed and cancelled proposals show then.
 */
export const completedProposals = (
  audit: AuditLogEntry[],
  pending: PendingProposals[],
  pendingLive = true,
): CompletedProposal[] => {
  const pendingKeys = new Set<string>();
  const selfKnown = new Set<string>();
  const domainKnown = new Set<string>();
  for (const party of pending) {
    if (pendingLive) {
      selfKnown.add(party.partyId);
      if (party.nextCursor === null) domainKnown.add(party.partyId);
    }
    for (const a of party.actions) pendingKeys.add(selfKey(party.partyId, a.action));
    for (const d of party.domainActions) {
      pendingKeys.add(domainKey(party.partyId, d.proposal_cid));
    }
  }

  const chronological = audit
    .filter((row) => row.status === "success")
    .sort((a, b) => a.timestamp - b.timestamp || a.id - b.id);

  interface Group {
    identity: Identity;
    partyId: string;
    rows: AuditLogEntry[];
  }
  const groups: Group[] = [];
  const open = new Map<string, Group>();
  for (const row of chronological) {
    const identity = identify(row);
    if (!identity) continue;
    let group = open.get(identity.key);
    if (!group) {
      group = { identity, partyId: row.party_id, rows: [] };
      groups.push(group);
      open.set(identity.key, group);
    }
    group.rows.push(row);
    // An identical self action proposed again later is a new proposal.
    if (TERMINAL_EVENTS.has(row.event_type)) open.delete(identity.key);
  }

  const completed: CompletedProposal[] = [];
  for (const { identity, partyId, rows } of groups) {
    if (pendingKeys.has(identity.key)) continue;
    const last = rows[rows.length - 1];
    let outcome: ProposalOutcome;
    if (last.event_type === "execute") {
      outcome = "executed";
    } else if (last.event_type === "cancel_proposal") {
      outcome = "cancelled";
    } else {
      const known =
        identity.kind === "self" ? selfKnown.has(partyId) : domainKnown.has(partyId);
      if (!known) continue;
      outcome = last.event_type === "propose" ? "proposed" : "confirmed";
    }
    completed.push({
      key: `${identity.key}|${rows[0].id}`,
      partyId,
      ts: last.timestamp,
      label: labelFor(identity, rows),
      outcome,
    });
  }
  return completed;
};
