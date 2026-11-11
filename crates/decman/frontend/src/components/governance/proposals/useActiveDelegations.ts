import { useCallback, useEffect, useState } from "react";
import { API_BASE } from "../../../constants";
import { authenticatedFetch } from "../../../api";
import type { ProposalFields } from "./useProposalFields";
import type {
  ActiveCouponReassignmentDelegation,
  CouponReassignmentDelegationSummary,
} from "../../../types";

/** The coupon reassignment delegations this party already has. */
export function useActiveDelegations(partyId: string, fields: ProposalFields) {
  const {
    proposalType,
    setProposalPriorDelegation,
    setProposalRevokeDelegationCid,
  } = fields;

  // The delegations this party already has, newest first, read from the ledger.
  // Normally zero or one — the singleton is not ledger-enforced, so several can
  // appear and both vote forms then offer a choice. Index 0 is the one the
  // automation acts on.
  const [activeDelegations, setActiveDelegations] = useState<
    CouponReassignmentDelegationSummary[]
  >([]);
  const [activeDelegationLoading, setActiveDelegationLoading] = useState(false);
  const [activeDelegationError, setActiveDelegationError] = useState<string | null>(null);

  // Read the delegations this party already has, so neither vote form asks for a
  // pasted contract id. Setup needs it for "Replaces Delegation" — blank while
  // one is live is rejected with 409. Revoke needs it to name what to archive.
  const fetchActiveDelegations = useCallback(async () => {
    setActiveDelegationLoading(true);
    setActiveDelegationError(null);
    try {
      const res = await authenticatedFetch(
        `${API_BASE}/coupon-reassignment-delegation?party_id=${encodeURIComponent(partyId)}`,
      );
      if (!res.ok) {
        setActiveDelegationError("Could not read this party's current delegations.");
        return;
      }
      const data: ActiveCouponReassignmentDelegation = await res.json();
      const active = data.delegations ?? [];
      setActiveDelegations(active);
      // Preselect the one the automation acts on. With several active a human
      // still has to choose, but the automation's pick is the sane default.
      if (active.length > 0) {
        setProposalPriorDelegation((cur) => cur || active[0].cid);
        setProposalRevokeDelegationCid((cur) => cur || active[0].cid);
      }
    } catch (e) {
      console.error("Failed to fetch active coupon reassignment delegations:", e);
      setActiveDelegationError("Could not read this party's current delegations.");
    } finally {
      setActiveDelegationLoading(false);
    }
  }, [partyId, setProposalPriorDelegation, setProposalRevokeDelegationCid]);

  useEffect(() => {
    if (
      proposalType === "setup_coupon_reassignment_delegation" ||
      proposalType === "revoke_coupon_reassignment_delegation"
    ) {
      fetchActiveDelegations();
    }
  }, [proposalType, fetchActiveDelegations]);

  return {
    activeDelegations,
    setActiveDelegations,
    activeDelegationLoading,
    setActiveDelegationLoading,
    activeDelegationError,
    setActiveDelegationError,
    fetchActiveDelegations,
  };
}
