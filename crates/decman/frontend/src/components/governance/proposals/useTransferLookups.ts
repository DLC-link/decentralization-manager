import { useCallback, useEffect, useState } from "react";
import { API_BASE } from "../../../constants";
import { authenticatedFetch } from "../../../api";
import { usePartyList } from "../../../hooks/usePartyList";
import type { ProposalFields } from "./useProposalFields";
import type {
  Holding,
  HoldingsResponse,
  TransferFactoriesResponse,
  TransferFactoryInfo,
  TransferInstructionsResponse,
} from "../../../types";

/**
 * The lookups of the Transfer and Accept Transfer forms: open transfer
 * instructions, holdings and factories, and the prefill from the picked
 * holding.
 */
export function useTransferLookups(partyId: string, fields: ProposalFields) {
  const {
    proposalType,
    setProposalTransferFactoryCid,
    setProposalExpectedAdmin,
    setProposalInstrumentIdAdmin,
    setProposalInstrumentIdId,
    selectedHoldingKey,
  } = fields;

  // Open TransferInstruction contracts addressed to this dec-party. Powers the
  // Accept Transfer proposal dropdown.
  const {
    items: openTransferInstructions,
    loading: transferInstructionsLoading,
    load: fetchOpenTransferInstructions,
  } = usePartyList<TransferInstructionsResponse>({
    path: `${API_BASE}/governance/transfer-instructions`,
    partyId,
    field: "transfer_instructions",
    what: "transfer instructions",
  });
  // Holdings + TransferFactory contracts power the Transfer Proposal form's
  // instrument dropdown. Holdings define which instruments the user can pick
  // (and the available balance); factories prefill the factory contract id +
  // expected admin once an instrument is selected (joined by
  // factory.expected_admin == holding.instrument_admin).
  const [transferHoldings, setTransferHoldings] = useState<Holding[]>([]);
  const [transferFactories, setTransferFactories] = useState<TransferFactoryInfo[]>([]);
  const [transferPrefillLoading, setTransferPrefillLoading] = useState(false);

  useEffect(() => {
    if (proposalType === "accept_transfer") {
      fetchOpenTransferInstructions();
    }
  }, [proposalType, fetchOpenTransferInstructions]);

  // Fetch holdings + transfer factories for the Transfer Proposal dropdown.
  // Both endpoints are cheap (one ACS query each) and we need them together
  // to render the dropdown + prefill, so fetch them in parallel.
  const fetchTransferPrefillData = useCallback(async () => {
    setTransferPrefillLoading(true);
    try {
      const [hRes, fRes] = await Promise.all([
        authenticatedFetch(
          `${API_BASE}/holdings?party_id=${encodeURIComponent(partyId)}`,
        ),
        authenticatedFetch(
          `${API_BASE}/transfer-factories?party_id=${encodeURIComponent(partyId)}`,
        ),
      ]);
      if (hRes.ok) {
        const data: HoldingsResponse = await hRes.json();
        setTransferHoldings(data.holdings);
      }
      if (fRes.ok) {
        const data: TransferFactoriesResponse = await fRes.json();
        setTransferFactories(data.transfer_factories);
      }
    } catch (e) {
      console.error("Failed to fetch transfer prefill data:", e);
    } finally {
      setTransferPrefillLoading(false);
    }
  }, [partyId]);

  useEffect(() => {
    if (proposalType === "transfer") {
      fetchTransferPrefillData();
    }
  }, [proposalType, fetchTransferPrefillData]);

  // Whenever the user picks an instrument from the dropdown, push its
  // identifiers and the matching factory into the (still-required) submission
  // fields. We keep those state vars so the existing submit path is
  // untouched — the form is just driven by `selectedHoldingKey` now.
  useEffect(() => {
    if (!selectedHoldingKey) return;
    const holding = transferHoldings.find(
      (h) => `${h.instrument_admin}::${h.instrument_id}` === selectedHoldingKey,
    );
    if (!holding) return;
    setProposalInstrumentIdAdmin(holding.instrument_admin);
    setProposalInstrumentIdId(holding.instrument_id);
    const factory = transferFactories.find(
      (f) => f.expected_admin === holding.instrument_admin,
    );
    if (factory) {
      setProposalTransferFactoryCid(factory.contract_id);
      setProposalExpectedAdmin(factory.expected_admin);
    } else {
      setProposalTransferFactoryCid("");
      setProposalExpectedAdmin(holding.instrument_admin);
    }
  }, [
    selectedHoldingKey,
    transferHoldings,
    transferFactories,
    setProposalInstrumentIdAdmin,
    setProposalInstrumentIdId,
    setProposalTransferFactoryCid,
    setProposalExpectedAdmin,
  ]);

  return {
    openTransferInstructions,
    transferInstructionsLoading,
    fetchOpenTransferInstructions,
    transferHoldings,
    setTransferHoldings,
    transferFactories,
    setTransferFactories,
    transferPrefillLoading,
    setTransferPrefillLoading,
    fetchTransferPrefillData,
  };
}
