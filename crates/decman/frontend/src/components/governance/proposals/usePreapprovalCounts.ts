import { useCallback, useEffect, useState } from "react";
import { API_BASE } from "../../../constants";
import { authenticatedFetch } from "../../../api";
import type { ProposalFields } from "./useProposalFields";
import type { TransferPreapprovalsResponse } from "../../../types";

/** How many TransferPreapprovals the party already has, for the setup forms. */
export function usePreapprovalCounts(partyId: string, fields: ProposalFields) {
  const {
    proposalType,
  } = fields;

  // Counts of active TransferPreapproval contracts the gov party already has
  // (CC + Token). Used to warn before issuing a Setup*Preapproval proposal
  // that would be a no-op when executed.
  const [preapprovalCounts, setPreapprovalCounts] = useState<TransferPreapprovalsResponse>({
    cc: 0,
    token: 0,
  });

  // Setup*Preapproval forms warn when one already exists — fetch the counts
  // (cheap, two ACS template-filter queries).
  const fetchPreapprovalCounts = useCallback(async () => {
    try {
      const res = await authenticatedFetch(
        `${API_BASE}/transfer-preapprovals?party_id=${encodeURIComponent(partyId)}`,
      );
      if (res.ok) {
        setPreapprovalCounts(await res.json());
      }
    } catch (e) {
      console.error("Failed to fetch transfer preapproval counts:", e);
    }
  }, [partyId]);

  useEffect(() => {
    if (
      proposalType === "setup_cc_preapproval" ||
      proposalType === "setup_token_preapproval"
    ) {
      fetchPreapprovalCounts();
    }
  }, [proposalType, fetchPreapprovalCounts]);

  return {
    preapprovalCounts,
    setPreapprovalCounts,
    fetchPreapprovalCounts,
  };
}
