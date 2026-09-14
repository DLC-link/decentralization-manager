import type { WorkflowKind } from "./types";

export interface WorkflowStepInfo {
  /** Matches the Rust `WorkflowStep::step_name()` for the variant. */
  name: string;
  label: string;
  description: string;
}

const WAITING_FOR_PEERS: WorkflowStepInfo = {
  name: "WaitingForPeers",
  label: "Waiting for members",
  description: "Every invited node must connect before the workflow can start.",
};

export const WORKFLOW_KIND_LABELS: Record<WorkflowKind, string> = {
  Onboarding: "Onboarding",
  Kick: "Kick",
  Contracts: "Contracts",
  Dars: "DARs",
  AddParty: "Add party",
  ChangeThreshold: "Change threshold",
};

export const WORKFLOW_STEPS: Record<WorkflowKind, WorkflowStepInfo[]> = {
  Onboarding: [
    WAITING_FOR_PEERS,
    {
      name: "GenerateKeys",
      label: "Generating keys",
      description:
        "Each member generates its share of the party's namespace and Daml keys.",
    },
    {
      name: "CreateProposals",
      label: "Creating proposals",
      description:
        "The coordinator builds the namespace and party-hosting proposals.",
    },
    {
      name: "SignDns",
      label: "Signing the namespace",
      description: "Every member signs the decentralized namespace proposal.",
    },
    {
      name: "SubmitDns",
      label: "Submitting the namespace",
      description:
        "The coordinator submits the signed namespace to the synchronizer.",
    },
    {
      name: "SignP2p",
      label: "Signing party hosting",
      description:
        "Every member signs the proposal that hosts the party on its participant.",
    },
    {
      name: "SubmitFinal",
      label: "Submitting party hosting",
      description:
        "The coordinator submits the hosting proposal, which brings the party live.",
    },
    {
      name: "Complete",
      label: "Complete",
      description: "The decentralized party exists and every member hosts it.",
    },
  ],
  Kick: [
    WAITING_FOR_PEERS,
    {
      name: "ExportState",
      label: "Reading the current state",
      description:
        "The coordinator exports the party's namespace and hosting state.",
    },
    {
      name: "CreateProposals",
      label: "Creating proposals",
      description:
        "The coordinator builds proposals that drop the removed member.",
    },
    {
      name: "SignProposals",
      label: "Signing proposals",
      description: "The remaining members sign the removal.",
    },
    {
      name: "SubmitKick",
      label: "Submitting the removal",
      description: "The coordinator submits the removal to the synchronizer.",
    },
    {
      name: "Complete",
      label: "Complete",
      description: "The removed member no longer hosts the party.",
    },
  ],
  Contracts: [
    WAITING_FOR_PEERS,
    {
      name: "PrepareSubmissions",
      label: "Preparing submissions",
      description:
        "The coordinator prepares the ledger commands for the members to sign.",
    },
    {
      name: "SignSubmissions",
      label: "Signing submissions",
      description: "Every member signs the prepared transactions.",
    },
    {
      name: "ExecuteSubmissions",
      label: "Executing submissions",
      description:
        "The coordinator submits the signed transactions to the ledger.",
    },
    {
      name: "Complete",
      label: "Complete",
      description: "The contracts are on the ledger.",
    },
  ],
  Dars: [
    WAITING_FOR_PEERS,
    {
      name: "UploadDars",
      label: "Uploading DARs",
      description:
        "Every member uploads the Daml packages to its participant and vets them.",
    },
    {
      name: "Complete",
      label: "Complete",
      description: "Every member has the packages.",
    },
  ],
  AddParty: [
    WAITING_FOR_PEERS,
    {
      name: "GenerateNewMemberKeys",
      label: "Generating the new member's keys",
      description:
        "The new member generates its namespace and Daml keys and sends the public halves.",
    },
    {
      name: "ExportState",
      label: "Reading the current state",
      description:
        "The coordinator exports the party's namespace and hosting state and checks that the member can join.",
    },
    {
      name: "CreateProposals",
      label: "Creating proposals",
      description:
        "The coordinator builds the namespace and hosting proposals that include the new member.",
    },
    {
      name: "SignProposals",
      label: "Signing proposals",
      description: "Every member signs both proposals.",
    },
    {
      name: "SubmitProposals",
      label: "Submitting proposals",
      description:
        "The coordinator submits the namespace and hosting changes, then exports the party's contracts.",
    },
    {
      name: "SyncAcs",
      label: "Copying contracts",
      description:
        "The new member imports the party's active contracts. Skipped when the party holds none.",
    },
    {
      name: "PrepareClearOnboarding",
      label: "Preparing to clear the onboarding flag",
      description:
        "The coordinator gives the new member what it needs to clear its onboarding flag.",
    },
    {
      name: "ProposeClearOnboarding",
      label: "Clearing the onboarding flag",
      description:
        "The new member waits out Canton's safe time, then clears its onboarding flag.",
    },
    {
      name: "PrepareClearSign",
      label: "Preparing the clearing proposal",
      description:
        "The coordinator builds the proposal that completes the onboarding.",
    },
    {
      name: "SignClearOnboarding",
      label: "Signing the clearing proposal",
      description: "Every member signs the proposal that completes the onboarding.",
    },
    {
      name: "SubmitClearOnboarding",
      label: "Submitting the clearing proposal",
      description:
        "The coordinator submits the proposal and waits for the onboarding flag to drop.",
    },
    {
      name: "Complete",
      label: "Complete",
      description: "The new member hosts the party.",
    },
  ],
  ChangeThreshold: [
    WAITING_FOR_PEERS,
    {
      name: "ExportState",
      label: "Reading the current state",
      description: "The coordinator exports the party's namespace state.",
    },
    {
      name: "CreateProposals",
      label: "Creating proposals",
      description:
        "The coordinator builds proposals that carry the new threshold.",
    },
    {
      name: "SignProposals",
      label: "Signing proposals",
      description: "A quorum of members signs the new threshold.",
    },
    {
      name: "Submit",
      label: "Submitting the change",
      description:
        "The coordinator submits the new threshold to the synchronizer.",
    },
    {
      name: "Complete",
      label: "Complete",
      description: "The party runs with the new threshold.",
    },
  ],
};

/**
 * Steps the backend writes that belong to no step enum. A peer row sits on
 * `"Active"` from the moment the invite is accepted until the coordinator's
 * first command lands.
 */
const SYNTHETIC_STEPS: Record<string, WorkflowStepInfo> = {
  Active: {
    name: "Active",
    label: "Waiting for the coordinator",
    description:
      "This node accepted the invite. The coordinator has not sent it a command yet.",
  },
};

const syntheticStep = (name: string): WorkflowStepInfo | undefined =>
  Object.hasOwn(SYNTHETIC_STEPS, name) ? SYNTHETIC_STEPS[name] : undefined;

/** `"SubmitClearOnboarding"` -> `"Submit clear onboarding"`. */
export const humanizeEnumName = (name: string): string =>
  name
    .replace(/([a-z0-9])([A-Z])/g, "$1 $2")
    .replace(/([A-Z]+)([A-Z][a-z])/g, "$1 $2")
    .split(" ")
    .map((word, i) =>
      i === 0 || /^[A-Z0-9]+$/.test(word) ? word : word.toLowerCase(),
    )
    .join(" ");

export const workflowKindLabel = (kind: WorkflowKind): string =>
  WORKFLOW_KIND_LABELS[kind] ?? humanizeEnumName(kind);

interface StepPosition {
  kind: WorkflowKind;
  current_step: string;
  step_index: number;
  step_total: number;
}

/**
 * The step list for a run, or `null` when it disagrees with the backend.
 *
 * `WORKFLOW_STEPS` is a hand-kept copy of the Rust step enums, so a step added
 * or reordered on the backend would mislabel every dot from there on. The run
 * carries both the total and the name of the step it sits on, which is enough
 * to detect a stale copy and fall back to unlabelled dots instead of confident
 * wrong labels: the name at `step_index` has to be the one the run reports, so
 * a renamed or replaced step is caught even when the total holds. A synthetic
 * step the backend writes outside any enum takes the place of the entry at its
 * index, so the dot describes the state the run is actually in.
 *
 * A step reordered further down the list than the run has reached is the one
 * case this cannot see, because no other step name is on the wire. Those
 * labels self-correct: the run advances into the changed step, the name stops
 * matching, and the dots go bare.
 */
export const stepsForRun = (run: StepPosition): WorkflowStepInfo[] | null => {
  const steps = WORKFLOW_STEPS[run.kind];
  if (!steps || steps.length !== run.step_total) return null;
  const synthetic = syntheticStep(run.current_step);
  if (synthetic) {
    // The run is not on the enum step this index names, so the dot has to
    // carry the synthetic step rather than describe one that is not running.
    return steps.map((s, i) => (i === run.step_index ? synthetic : s));
  }
  return steps[run.step_index]?.name === run.current_step ? steps : null;
};

/** Human label for the step a run sits on, PascalCase name as the fallback. */
export const currentStepLabel = (run: StepPosition): string =>
  stepsForRun(run)?.find((s) => s.name === run.current_step)?.label ??
  syntheticStep(run.current_step)?.label ??
  humanizeEnumName(run.current_step);
