import { useCallback, useEffect, useState } from "react";
import { API_BASE } from "../../../constants";
import { authenticatedFetch } from "../../../api";
import type { ProposalFields } from "./useProposalFields";
import type { NetworkInfo } from "../../../types";

/**
 * The DSO party and AmuletRules read from network-info, prefilled into the
 * forms that need them.
 */
export function useNetworkInfo(fields: ProposalFields) {
  const {
    proposalType,
    setProposalExpectedDso,
    setProposalDelegationDso,
    amuletRulesCid,
    setAmuletRulesCid,
    dsoPartyId,
    setDsoPartyId,
  } = fields;

  const [amuletRulesLoading, setAmuletRulesLoading] = useState(false);

  // Fetch network info (DSO party + amulet rules) from DSO API
  const fetchNetworkInfo = useCallback(async () => {
    setAmuletRulesLoading(true);
    try {
      const res = await authenticatedFetch(`${API_BASE}/network-info`);
      if (res.ok) {
        const data: NetworkInfo = await res.json();
        setAmuletRulesCid(data.amulet_rules_cid);
        setDsoPartyId(data.dso_party_id);
        setProposalExpectedDso(data.dso_party_id);
        // The coupon-reassignment form keeps its DSO in its own state, so it
        // needs prefilling here too. A wrong DSO there assigns nothing at all,
        // silently, which is indistinguishable from having nothing to do.
        setProposalDelegationDso(data.dso_party_id);
      }
    } catch (e) {
      console.error("Failed to fetch network info:", e);
    } finally {
      setAmuletRulesLoading(false);
    }
  }, [setAmuletRulesCid, setDsoPartyId, setProposalExpectedDso, setProposalDelegationDso]);

  // Setup CC Preapproval and Setup Minting Delegation need the DSO party id
  // from the network-info endpoint to prefill their DSO field. Mirror the
  // action-form trigger above for the proposal form.
  useEffect(() => {
    if (
      (proposalType === "setup_cc_preapproval" ||
        proposalType === "setup_minting_delegation" ||
        proposalType === "setup_coupon_reassignment_delegation") &&
      !dsoPartyId
    ) {
      fetchNetworkInfo();
    }
  }, [proposalType, dsoPartyId, fetchNetworkInfo]);

  // Request DevNet Featured App Right and Accept Paid Credential pin the
  // DSO's current AmuletRules, so prefill its cid from network-info too. The
  // field keeps a refresh button for when the DSO replaces the contract while
  // the form is open.
  useEffect(() => {
    if (
      (proposalType === "request_dev_net_featured_app_right" ||
        proposalType === "accept_paid_credential") &&
      !amuletRulesCid
    ) {
      fetchNetworkInfo();
    }
  }, [proposalType, amuletRulesCid, fetchNetworkInfo]);

  return {
    amuletRulesLoading,
    setAmuletRulesLoading,
    fetchNetworkInfo,
  };
}
