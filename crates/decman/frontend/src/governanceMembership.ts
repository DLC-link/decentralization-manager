import type { GovernanceState } from "./types";

export type GovernanceMembership = "member" | "not_member" | "unknown";

/**
 * Whether the node's configured member party sits in the rules contract's
 * member set. Compares full party ids: a truncated compare reports a false
 * mismatch. `unknown` when the governance state did not load, the party has
 * no rules contract, or the member set is empty: the ledger cannot hold an
 * empty set, so an empty list means the backend failed to decode it.
 */
export const getGovernanceMembership = (
  memberPartyId: string | undefined,
  governanceState: GovernanceState | null | undefined,
): GovernanceMembership => {
  if (!memberPartyId || !governanceState?.members.length) return "unknown";
  return governanceState.members.includes(memberPartyId)
    ? "member"
    : "not_member";
};
