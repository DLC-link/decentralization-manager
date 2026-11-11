import { useState, useEffect, useCallback } from "react";
import { Box, CircularProgress, Alert, Collapse } from "@mui/material";
import { API_BASE } from "../constants";
import { authenticatedFetch } from "../api";
import { ActionForm } from "./governance/actions/ActionForm";
import { useActionFormState } from "./governance/actions/useActionFormState";
import { ProposalForm } from "./governance/proposals/ProposalForm";
import { useProposalState } from "./governance/proposals/useProposalState";
import type {
  GovernanceResponse,
  Network,
  GovernanceState,
  GovernanceStateResponse,
} from "../types";

interface GovernanceSectionProps {
  partyId: string;
  rulesContractId?: string;
  defaultOperatorParty?: string;
  network?: Network;
  /// Called after every successful mutating action (propose / confirm /
  /// execute / revoke / expire / domain confirm / domain execute) so the
  /// parent can refresh sibling views (e.g. the audit trail tab).
  onAfterAction?: () => void;
  /// Called after a domain proposal is successfully created. The hosting
  /// dialog wires this to its `onClose` so the modal disappears on success;
  /// fires after `onAfterAction` so refreshes still run.
  onProposalCreated?: () => void;
  /// Which half of the section to render:
  /// - "actions"   = governance-action confirmations + new-action form
  /// - "proposals" = domain-proposal list + new-proposal form
  view: "actions" | "proposals";
  /// When provided, the inline Submit Confirmation / Submit Proposal button
  /// is rendered into this DOM node (via `Portal`) instead of inline at the
  /// bottom of the form. Used by `GovernanceActionsDialog` to lift the
  /// primary action into its `DialogActions` footer next to Close.
  submitPortalEl?: HTMLElement | null;
}

export const GovernanceSection = ({
  partyId,
  rulesContractId: initialRulesContractId,
  defaultOperatorParty,
  network,
  onAfterAction,
  onProposalCreated,
  view,
  submitPortalEl,
}: GovernanceSectionProps) => {
  const showActionsHalf = view === "actions";
  const showProposalsHalf = view === "proposals";
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [data, setData] = useState<GovernanceResponse | null>(null);
  const [rulesContractId, setRulesContractId] = useState(
    initialRulesContractId || "",
  );

  // Latest applied governance values (threshold from /governance/confirmations,
  // timeout from /governance/state). Used to prefill the new-action form so it
  // opens with the current values, not hardcoded 2 / 1h.
  const [governanceState, setGovernanceState] =
    useState<GovernanceState | null>(null);

  // Update rulesContractId when prop changes
  useEffect(() => {
    if (initialRulesContractId) {
      setRulesContractId(initialRulesContractId);
    }
  }, [initialRulesContractId]);

  const fetchGovernance = useCallback(async () => {
    try {
      const res = await authenticatedFetch(
        `${API_BASE}/governance/confirmations?party_id=${encodeURIComponent(partyId)}`,
      );
      if (res.ok) {
        const response: GovernanceResponse = await res.json();
        setData(response);
        if (response.rules_contract_id) {
          setRulesContractId(response.rules_contract_id);
        }
        setError(null);
      } else {
        const errData = await res.json().catch(() => ({}));
        setError(errData.error || "Failed to fetch governance data");
      }
    } catch (e) {
      setError(
        e instanceof Error ? e.message : "Failed to fetch governance data",
      );
    } finally {
      setLoading(false);
    }
  }, [partyId]);

  useEffect(() => {
    fetchGovernance();
    const interval = setInterval(fetchGovernance, 10000); // Poll every 10 seconds
    return () => clearInterval(interval);
  }, [fetchGovernance]);

  // Fetch governance state for action_confirmation_timeout_microseconds.
  // Threshold also comes back here, but the form already uses the threshold
  // from `data` (the confirmations payload) — `governanceState` is primarily
  // for the timeout field's prefill.
  const fetchGovernanceStateForPrefill = useCallback(async () => {
    try {
      const res = await authenticatedFetch(
        `${API_BASE}/governance/state?party_id=${encodeURIComponent(partyId)}`,
      );
      if (!res.ok) return;
      const body: GovernanceStateResponse = await res.json();
      setGovernanceState(body.state ?? null);
    } catch {
      /* fall back to hardcoded defaults */
    }
  }, [partyId]);

  useEffect(() => {
    fetchGovernanceStateForPrefill();
  }, [fetchGovernanceStateForPrefill]);

  const actionForm = useActionFormState(governanceState);
  const proposalState = useProposalState({ partyId, defaultOperatorParty });

  if (loading && !data) {
    return (
      <Box sx={{ display: "flex", justifyContent: "center", p: 2 }}>
        <CircularProgress size={24} />
      </Box>
    );
  }

  return (
    <Box sx={{ mt: 2 }}>
      {/* Shared across both halves: the proposals view (`view="proposals"`)
          does not render `showActionsHalf`, so keeping the error here ensures
          a failed `/governance/propose` (or action) surfaces in either view
          instead of failing silently. */}
      {error && (
        <Alert severity="error" sx={{ mb: 2 }} onClose={() => setError(null)}>
          {error}
        </Alert>
      )}

      {showActionsHalf && (
      <>
      <Collapse in>
        {(data?.gov_core_out_of_date || governanceState?.out_of_date) && (
          <Alert severity="warning" sx={{ mb: 2 }}>
            The governance core contract is out of date
            {data?.gov_core_package_ref || governanceState?.package_ref
              ? ` (running on ${data?.gov_core_package_ref || governanceState?.package_ref})`
              : ""}
            . Actions are executed against the old package — the party should
            be migrated to the latest governance-core package.
          </Alert>
        )}

        {/* New Action Form */}
        <ActionForm
          form={actionForm}
          partyId={partyId}
          rulesContractId={rulesContractId}
          network={network}
          governanceState={governanceState}
          setError={setError}
          fetchGovernance={fetchGovernance}
          fetchGovernanceStateForPrefill={fetchGovernanceStateForPrefill}
          onAfterAction={onAfterAction}
          submitPortalEl={submitPortalEl}
        />

      </Collapse>
      </>
      )}

      {/* Proposals — only for governance-core */}
      {showProposalsHalf && data && (
        <ProposalForm
          state={proposalState}
          rulesContractId={rulesContractId}
          network={network}
          setError={setError}
          fetchGovernance={fetchGovernance}
          onAfterAction={onAfterAction}
          onProposalCreated={onProposalCreated}
          submitPortalEl={submitPortalEl}
        />
      )}

    </Box>
  );
};
