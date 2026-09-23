import type { GovernanceState } from "./types";

export type GovernanceMembership = "member" | "not_member" | "unknown";

/**
 * Whether the node's configured member party sits in the rules contract's
 * member set. Compares full party ids: a truncated compare reports a false
 * mismatch. `unknown` when the governance state did not load or the party has
 * no rules contract.
 */
export const getGovernanceMembership = (
  memberPartyId: string | undefined,
  governanceState: GovernanceState | null | undefined,
): GovernanceMembership => {
  if (!memberPartyId || !governanceState) return "unknown";
  return governanceState.members.includes(memberPartyId)
    ? "member"
    : "not_member";
};
