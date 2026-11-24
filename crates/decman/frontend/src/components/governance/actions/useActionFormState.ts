import { useEffect, useRef, useState } from "react";
import type { ActionType, GovernanceState } from "../../../types";

type ActionTypeKey = ActionType["type"];

/**
 * The fields of the new-action form. The section holds them for its whole
 * life, so a value seeded from the governance state is in place before the
 * form first renders.
 */
const useActionFormState = (governanceState: GovernanceState | null) => {
  // Action form state
  const [selectedActionType, setSelectedActionType] = useState<ActionTypeKey>(
    "governance_add_member",
  );
  const [formLoading, setFormLoading] = useState(false);

  // Form fields for various action types
  const [memberParty, setMemberParty] = useState("");
  const [newThreshold, setNewThreshold] = useState(2);
  const [timeoutMicroseconds, setTimeoutMicroseconds] = useState(3600000000);
  // Once the user types into a threshold/timeout field we stop auto-seeding
  // from server state — otherwise the 10s poll would clobber their input.
  // `resetActionForm` flips these back to false so the next form opening
  // re-seeds from the latest applied values.
  const userEditedThresholdRef = useRef(false);
  const userEditedTimeoutRef = useRef(false);
  const userEditedObserversRef = useRef(false);
  // One party id per line. The action replaces the whole set, so it starts
  // from the current observers.
  const [disclosureObserversText, setDisclosureObserversText] = useState("");

  // Seed `newThreshold` from the active GovernanceRules contract once state
  // arrives. NOTE: do not use `data.threshold` here — that field on the
  // `/governance/confirmations` response is the decentralized-namespace
  // topology threshold (e.g. 2-of-3 owners), not the governance-rules
  // threshold. They are usually different numbers. The ref guard prevents
  // polling/refreshes from clobbering the user's typed value mid-edit.
  useEffect(() => {
    if (
      governanceState?.threshold != null &&
      !userEditedThresholdRef.current
    ) {
      setNewThreshold(Number(governanceState.threshold));
    }
  }, [governanceState?.threshold]);

  // Same pattern for the action confirmation timeout.
  useEffect(() => {
    const us = governanceState?.action_confirmation_timeout_microseconds;
    if (us != null && !userEditedTimeoutRef.current) {
      setTimeoutMicroseconds(us);
    }
  }, [governanceState?.action_confirmation_timeout_microseconds]);

  // And for the disclosure observers.
  const currentObserversText = (governanceState?.disclosure_observers ?? []).join("\n");
  useEffect(() => {
    if (!userEditedObserversRef.current) {
      setDisclosureObserversText(currentObserversText);
    }
  }, [currentObserversText]);

  return {
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
  };
};

type ActionFormState = ReturnType<typeof useActionFormState>;

export { useActionFormState };
export type { ActionFormState, ActionTypeKey };
