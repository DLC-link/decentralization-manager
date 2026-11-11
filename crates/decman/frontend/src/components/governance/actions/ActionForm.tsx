import {
  Box,
  Collapse,
  Divider,
  FormControl,
  InputLabel,
  MenuItem,
  Select,
} from "@mui/material";
import { API_BASE } from "../../../constants";
import { authenticatedFetch } from "../../../api";
import { getActionTypeOptions } from "../../../governanceFormat";
import { TextHelp } from "../../FieldHelp";
import { HelpTextField } from "../fields/HelpTextField";
import { SubmitButton } from "../fields/SubmitButton";
import { parsePartyLines } from "../proposalParse";
import type { ActionFormState, ActionTypeKey } from "./useActionFormState";
import type {
  ActionType,
  ConfirmActionRequest,
  GovernanceState,
  Network,
} from "../../../types";

interface ActionFormProps {
  /** The form fields, from useActionFormState. */
  form: ActionFormState;
  partyId: string;
  rulesContractId: string;
  network?: Network;
  governanceState: GovernanceState | null;
  setError: (error: string | null) => void;
  fetchGovernance: () => Promise<void>;
  fetchGovernanceStateForPrefill: () => Promise<void>;
  onAfterAction?: () => void;
  submitPortalEl?: HTMLElement | null;
}

/** The new-action form: one of the five self-management actions. */
export const ActionForm = ({
  form,
  partyId,
  rulesContractId,
  network,
  governanceState,
  setError,
  fetchGovernance,
  fetchGovernanceStateForPrefill,
  onAfterAction,
  submitPortalEl,
}: ActionFormProps) => {
  const {
    selectedActionType,
    setSelectedActionType,
    formLoading,
    setFormLoading,
    memberParty,
    setMemberParty,
    newThreshold,
    setNewThreshold,
    timeoutMicroseconds,
    setTimeoutMicroseconds,
    userEditedThresholdRef,
    userEditedTimeoutRef,
    userEditedObserversRef,
    disclosureObserversText,
    setDisclosureObserversText,
    currentObserversText,
  } = form;

  // Build ActionType from form state
  const buildActionFromForm = (): ActionType | null => {
    switch (selectedActionType) {
      case "governance_add_member":
        return {
          type: "governance_add_member",
          member: memberParty,
          new_threshold: newThreshold,
        };
      case "governance_remove_member":
        return {
          type: "governance_remove_member",
          member: memberParty,
          new_threshold: newThreshold,
        };
      case "governance_set_threshold":
        return {
          type: "governance_set_threshold",
          new_threshold: newThreshold,
        };
      case "governance_set_timeout":
        return {
          type: "governance_set_timeout",
          new_timeout_microseconds: timeoutMicroseconds,
        };
      case "governance_set_disclosure_observers": {
        // Sorted like the backend's party set, so the audit entry and the
        // ledger action list the parties in the same order.
        const observers = parsePartyLines(disclosureObserversText).sort();
        return {
          type: "governance_set_disclosure_observers",
          // Leaving the field out sends None, which clears the observers.
          disclosure_observers: observers.length > 0 ? observers : undefined,
        };
      }
      default:
        return null;
    }
  };

  // Clear the action form fields after a successful submit so the next
  // action starts blank — keeps the form expanded and the submit button
  // visible (the new action shows up in the notification queue on its own).
  // NOTE: dsoPartyId / amuletRulesCid are intentionally NOT
  // cleared — they're autofetched (or seeded once) and should persist across
  // submissions of the same dialog session.
  const resetActionForm = () => {
    setMemberParty("");
    // Reset to latest applied governance values rather than hardcoded
    // 2 / 1h — the next form opening should reflect on-chain state. Clearing
    // the edit refs also lets the auto-seed effects fire again for any
    // values that haven't loaded yet at reset time.
    userEditedThresholdRef.current = false;
    userEditedTimeoutRef.current = false;
    userEditedObserversRef.current = false;
    setDisclosureObserversText(currentObserversText);
    setNewThreshold(
      governanceState?.threshold != null
        ? Number(governanceState.threshold)
        : 2,
    );
    setTimeoutMicroseconds(
      governanceState?.action_confirmation_timeout_microseconds ?? 3600000000,
    );
  };

  const handleSubmitAction = async () => {
    if (!rulesContractId) {
      setError("Please enter the Governance contract ID");
      return;
    }

    let action: ActionType | null;
    try {
      action = buildActionFromForm();
    } catch (e) {
      setError(e instanceof Error ? e.message : "Invalid action");
      return;
    }
    if (!action) {
      setError("Invalid action type");
      return;
    }

    setFormLoading(true);
    setError(null);

    try {
      const request: ConfirmActionRequest = {
        party_id: partyId,
        rules_contract_id: rulesContractId,
        action,
        governance_type: "core_self",
      };

      const res = await authenticatedFetch(`${API_BASE}/governance/confirm`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify(request),
      });

      if (!res.ok) {
        const errData = await res.json().catch(() => ({}));
        throw new Error(errData.error || "Failed to submit confirmation");
      }

      // Clear fields, keep the form visible. The created action shows up
      // in the notification queue — no separate success message needed.
      resetActionForm();
      await Promise.all([fetchGovernance(), fetchGovernanceStateForPrefill()]);
      onAfterAction?.();
    } catch (e) {
      setError(
        e instanceof Error ? e.message : "Failed to submit confirmation",
      );
    } finally {
      setFormLoading(false);
    }
  };

  // Render form fields based on selected action type
  const renderActionFormFields = () => {
    switch (selectedActionType) {
      case "governance_add_member":
        return (
          <>
            <HelpTextField
              label="Member Party ID"
              value={memberParty}
              onChange={(e) => setMemberParty(e.target.value)}
              size="small"
              fullWidth
              sx={{ mb: 2 }}
              help="Party id of the new governance member to add to this rules contract."
            />
            <HelpTextField
              label="New Threshold"
              type="number"
              value={newThreshold}
              onChange={(e) => {
                userEditedThresholdRef.current = true;
                setNewThreshold(parseInt(e.target.value) || 2);
              }}
              size="small"
              fullWidth
              help="Number of governance member confirmations required to execute an action after this member is added."
            />
          </>
        );
      case "governance_remove_member": {
        // Source of truth for the current member list is the active rules
        // contract — `governanceState.members` is populated by the
        // /governance/state fetch. If it hasn't loaded yet (or for some
        // reason returns empty), fall back to the freeform text field so
        // the user isn't blocked.
        const members = governanceState?.members ?? [];
        return (
          <>
            {members.length > 0 ? (
              <HelpTextField
                select
                label="Member to remove"
                value={memberParty}
                onChange={(e) => setMemberParty(e.target.value)}
                size="small"
                fullWidth
                sx={{ mb: 2 }}
                help="Pick which existing governance member to remove from this rules contract."
              >
                {members.map((id) => (
                  <MenuItem key={id} value={id}>
                    {id}
                  </MenuItem>
                ))}
              </HelpTextField>
            ) : (
              <HelpTextField
                label="Member Party ID"
                value={memberParty}
                onChange={(e) => setMemberParty(e.target.value)}
                size="small"
                fullWidth
                sx={{ mb: 2 }}
                helperText="Members list not loaded — paste the party id directly"
                help="Party id of the governance member to remove."
              />
            )}
            <HelpTextField
              label="New Threshold"
              type="number"
              value={newThreshold}
              onChange={(e) => {
                userEditedThresholdRef.current = true;
                setNewThreshold(parseInt(e.target.value) || 2);
              }}
              size="small"
              fullWidth
              help="Number of governance member confirmations required to execute an action after this member is removed."
            />
          </>
        );
      }
      case "governance_set_threshold":
        return (
          <HelpTextField
            label="New Threshold"
            type="number"
            value={newThreshold}
            onChange={(e) => {
              userEditedThresholdRef.current = true;
              setNewThreshold(parseInt(e.target.value) || 2);
            }}
            size="small"
            fullWidth
            help="Number of governance member confirmations required to execute an action."
          />
        );
      case "governance_set_timeout":
        return (
          <HelpTextField
            label="Timeout (microseconds)"
            type="number"
            value={timeoutMicroseconds}
            onChange={(e) => {
              userEditedTimeoutRef.current = true;
              setTimeoutMicroseconds(parseInt(e.target.value) || 0);
            }}
            size="small"
            fullWidth
            helperText="1 hour = 3,600,000,000 microseconds"
            help="How long a confirmation stays valid before expiring, in microseconds. 1 hour = 3,600,000,000."
            helpLabel="Timeout"
          />
        );
      case "governance_set_disclosure_observers":
        return (
          <HelpTextField
            label="Disclosure Observers (one party id per line)"
            value={disclosureObserversText}
            onChange={(e) => {
              userEditedObserversRef.current = true;
              setDisclosureObserversText(e.target.value);
            }}
            size="small"
            fullWidth
            multiline
            minRows={2}
            maxRows={6}
            helperText="This list replaces the current one. Leave it empty to remove every observer."
            help="Parties that can see the rules contract without hosting the governance party. Observers cannot confirm or execute actions."
            helpLabel="Disclosure Observers"
          />
        );
      default:
        return null;
    }
  };

  return (
    <Box sx={{ mb: 2 }}>
      <Collapse in>
        <Box>
          <FormControl fullWidth size="small" sx={{ mb: 2 }}>
            <InputLabel>
              <TextHelp text="What kind of governance action to create. The fields below adapt to the selected type.">
                Action Type
              </TextHelp>
            </InputLabel>
            <Select
              value={selectedActionType}
              label="Action Type"
              onChange={(e) =>
                setSelectedActionType(e.target.value as ActionTypeKey)
              }
              MenuProps={{ disableScrollLock: true }}
            >
              {getActionTypeOptions(network).filter((opt) => {
                // For governance-core, only show self-management actions
                const selfActions = ["governance_add_member", "governance_remove_member", "governance_set_threshold", "governance_set_timeout", "governance_set_disclosure_observers"];
                return selfActions.includes(opt.value);
              }).map(
                (opt) => (
                  <MenuItem key={opt.value} value={opt.value}>
                    {opt.label}
                  </MenuItem>
                ),
              )}
            </Select>
          </FormControl>

          <Divider sx={{ my: 2 }} />

          {renderActionFormFields()}

          <SubmitButton
            label="Submit Confirmation"
            loading={formLoading}
            disabled={formLoading || !rulesContractId}
            onClick={handleSubmitAction}
            portalEl={submitPortalEl}
            inlineBoxSx={{ mt: 2, display: "flex", gap: 1 }}
          />
        </Box>
      </Collapse>
    </Box>
  );
};
