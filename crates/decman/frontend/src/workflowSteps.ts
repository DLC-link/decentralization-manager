import type { MemberVariant, WorkflowKind, WorkflowRole } from "./types";

export interface WorkflowStepInfo {
  /** Matches the step name the Rust driver writes (`engine::steps_for`). */
  name: string;
  label: string;
  description: string;
}

/**
 * The coordinator waits here until every invitee it needs has accepted the
 * `WorkflowProposal` on the ledger.
 */
const WAITING_FOR_ACCEPTANCES: WorkflowStepInfo = {
  name: "WaitingForAcceptances",
  label: "Waiting for acceptances",
  description:
    "Every invited node must accept the proposal on the ledger before the workflow can start.",
};

const COMPLETE = (description: string): WorkflowStepInfo => ({
  name: "Complete",
  label: "Complete",
  description,
});

export const WORKFLOW_KIND_LABELS: Record<WorkflowKind, string> = {
  Onboarding: "Onboarding",
  Kick: "Kick",
  Contracts: "Contracts",
  Dars: "DARs",
  AddParty: "Add party",
  ChangeThreshold: "Change threshold",
};

/**
 * Step lists, keyed the way the backend picks them: by kind, by role, and for
 * `AddParty` by which member list a peer follows. A peer row and a coordinator
 * row of the same run walk different lists of different lengths, so one list
 * per kind cannot describe both.
 */
type RoleSteps = {
  Coordinator: WorkflowStepInfo[];
  /** Peer rows. `Joiner` only exists where a kind splits its member list. */
  Member: WorkflowStepInfo[];
  Joiner?: WorkflowStepInfo[];
};

export const WORKFLOW_STEPS: Record<WorkflowKind, RoleSteps> = {
  Onboarding: {
    Coordinator: [
      {
        name: "GenerateKeys",
        label: "Generating keys",
        description:
          "The coordinator generates its key for the party and publishes the root delegation.",
      },
      WAITING_FOR_ACCEPTANCES,
      {
        name: "ProposeNamespace",
        label: "Proposing the namespace",
        description:
          "The coordinator proposes the decentralized namespace to the synchronizer.",
      },
      {
        name: "AwaitNamespace",
        label: "Waiting for the namespace",
        description:
          "Every member co-signs the namespace proposal, then it becomes effective.",
      },
      {
        name: "ProposeParty",
        label: "Proposing party hosting",
        description:
          "The coordinator proposes the mapping that hosts the party on every participant.",
      },
      {
        name: "AwaitParty",
        label: "Waiting for party hosting",
        description:
          "Every member co-signs the hosting proposal, then the party goes live.",
      },
      COMPLETE("The decentralized party exists and every member hosts it."),
    ],
    Member: [
      {
        name: "GenerateKeys",
        label: "Generating keys",
        description:
          "This node generates its key for the party and accepts the proposal with the public half.",
      },
      {
        name: "CoSignNamespace",
        label: "Co-signing the namespace",
        description:
          "This node validates the namespace proposal against the invite, then co-signs it.",
      },
      {
        name: "CoSignParty",
        label: "Co-signing party hosting",
        description:
          "This node validates the hosting proposal, then co-signs it.",
      },
      COMPLETE("The decentralized party exists and this node hosts it."),
    ],
  },
  Kick: {
    Coordinator: [
      WAITING_FOR_ACCEPTANCES,
      {
        name: "ProposeChanges",
        label: "Proposing the removal",
        description:
          "The coordinator proposes the namespace that drops the removed member.",
      },
      {
        name: "AwaitChanges",
        label: "Waiting for the removal",
        description:
          "The remaining members co-sign the namespace and the hosting change.",
      },
      COMPLETE("The removed member no longer owns or hosts the party."),
    ],
    Member: [
      {
        name: "CoSignChanges",
        label: "Co-signing the removal",
        description:
          "This node validates both proposals against the invite, then co-signs them.",
      },
      COMPLETE("The removed member no longer owns or hosts the party."),
    ],
  },
  Contracts: {
    Coordinator: [
      WAITING_FOR_ACCEPTANCES,
      {
        name: "AwaitDars",
        label: "Waiting for packages",
        description:
          "Every participant must vet the pinned packages before a command can run.",
      },
      {
        name: "PrepareSubmissions",
        label: "Preparing submissions",
        description:
          "The coordinator prepares one transaction per contract and opens a signing round for each.",
      },
      {
        name: "CollectSignatures",
        label: "Collecting signatures",
        description:
          "Each round collects member signatures until it reaches the party threshold.",
      },
      {
        name: "ExecuteSubmissions",
        label: "Executing submissions",
        description:
          "The coordinator submits each signed transaction to the ledger.",
      },
      COMPLETE("The contracts are on the ledger."),
    ],
    Member: [
      {
        name: "UploadDars",
        label: "Vetting packages",
        description:
          "This node uploads and vets every pinned package before it signs.",
      },
      {
        name: "SignSubmissions",
        label: "Signing submissions",
        description:
          "This node checks each prepared transaction, then signs its hash with the party key.",
      },
      COMPLETE("The contracts are on the ledger."),
    ],
  },
  Dars: {
    Coordinator: [
      WAITING_FOR_ACCEPTANCES,
      {
        name: "AwaitVetting",
        label: "Waiting for vetting",
        description:
          "Every participant must vet the pinned packages, which the topology store reports.",
      },
      COMPLETE("Every member has the packages."),
    ],
    Member: [
      {
        name: "UploadDars",
        label: "Uploading DARs",
        description:
          "This node uploads the pinned packages to its participant and vets them.",
      },
      COMPLETE("This node has the packages."),
    ],
  },
  AddParty: {
    Coordinator: [
      {
        name: "GenerateKeys",
        label: "Generating keys",
        description:
          "The coordinator is already a member, so it has no new key to make.",
      },
      WAITING_FOR_ACCEPTANCES,
      {
        name: "ProposeChanges",
        label: "Proposing the changes",
        description:
          "The coordinator proposes the namespace and the hosting mapping that include the new member.",
      },
      {
        name: "AwaitChanges",
        label: "Waiting for the changes",
        description:
          "Every member co-signs both proposals, then the new member hosts the party.",
      },
      {
        name: "AwaitReplication",
        label: "Copying contracts",
        description:
          "Each current host publishes a manifest of the party's contracts for the new member to import.",
      },
      COMPLETE("The new member hosts the party."),
    ],
    Joiner: [
      {
        name: "GenerateKeys",
        label: "Generating keys",
        description:
          "The new member generates its key for the party and publishes the root delegation.",
      },
      {
        name: "CoSignChanges",
        label: "Co-signing the changes",
        description:
          "The new member validates both proposals against the invite, then co-signs them.",
      },
      {
        name: "SyncAcs",
        label: "Importing contracts",
        description:
          "The new member imports the party's active contracts. It skips this when the party holds none.",
      },
      {
        name: "ClearOnboarding",
        label: "Clearing the onboarding flag",
        description:
          "The new member clears its own onboarding flag, which needs no co-signature.",
      },
      COMPLETE("The new member hosts the party."),
    ],
    Member: [
      {
        name: "CoSignChanges",
        label: "Co-signing the changes",
        description:
          "This node validates both proposals against the invite, then co-signs them.",
      },
      {
        name: "PublishManifest",
        label: "Publishing a manifest",
        description:
          "This node exports the party's contracts and publishes a manifest that pins the file.",
      },
      COMPLETE("The new member hosts the party."),
    ],
  },
  ChangeThreshold: {
    Coordinator: [
      WAITING_FOR_ACCEPTANCES,
      {
        name: "ProposeChanges",
        label: "Proposing the new threshold",
        description:
          "The coordinator proposes the namespace that carries the new threshold.",
      },
      {
        name: "AwaitChanges",
        label: "Waiting for the new threshold",
        description:
          "A quorum of members co-signs the namespace and the hosting change.",
      },
      COMPLETE("The party runs with the new threshold."),
    ],
    Member: [
      {
        name: "CoSignChanges",
        label: "Co-signing the new threshold",
        description:
          "This node validates both proposals against the invite, then co-signs them.",
      },
      COMPLETE("The party runs with the new threshold."),
    ],
  },
};

/**
 * Steps the backend writes that belong to no step list. Rows created before
 * the 2.0 upgrade can still sit on `"Active"`.
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

/** `"WaitingForAcceptances"` -> `"Waiting for acceptances"`. */
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
  role: WorkflowRole;
  /** `null` on coordinator rows and on kinds with one member step list. */
  member_variant?: MemberVariant | null;
  current_step: string;
  step_index: number;
  step_total: number;
}

/** The list the backend walks for this run's kind, role, and variant. */
const listFor = (run: StepPosition): WorkflowStepInfo[] | undefined => {
  const byRole = WORKFLOW_STEPS[run.kind];
  if (!byRole) return undefined;
  if (run.role === "Coordinator") return byRole.Coordinator;
  return run.member_variant === "Joiner"
    ? (byRole.Joiner ?? byRole.Member)
    : byRole.Member;
};

/**
 * The step list for a run, or `null` when it disagrees with the backend.
 *
 * `WORKFLOW_STEPS` is a hand-kept copy of the Rust step lists, so a step added
 * or reordered on the backend would mislabel every dot from there on. The run
 * carries both the total and the name of the step it sits on, which is enough
 * to detect a stale copy and fall back to unlabelled dots instead of confident
 * wrong labels: the name at `step_index` has to be the one the run reports, so
 * a renamed or replaced step is caught even when the total holds. A synthetic
 * step the backend writes outside any list takes the place of the entry at its
 * index, so the dot describes the state the run is actually in.
 *
 * A step reordered further down the list than the run has reached is the one
 * case this cannot see, because no other step name is on the wire. Those
 * labels self-correct: the run advances into the changed step, the name stops
 * matching, and the dots go bare.
 */
export const stepsForRun = (run: StepPosition): WorkflowStepInfo[] | null => {
  const steps = listFor(run);
  if (!steps || steps.length !== run.step_total) return null;
  const at = steps[run.step_index];
  if (!at) return null;
  const synthetic = syntheticStep(run.current_step);
  // The run is not on the step its index names, so the dot has to carry the
  // synthetic step rather than describe one that is not running.
  if (synthetic) {
    return steps.map((s, i) => (i === run.step_index ? synthetic : s));
  }
  return at.name === run.current_step ? steps : null;
};

/** Human label for the step a run sits on, PascalCase name as the fallback. */
export const currentStepLabel = (run: StepPosition): string =>
  stepsForRun(run)?.find((s) => s.name === run.current_step)?.label ??
  syntheticStep(run.current_step)?.label ??
  humanizeEnumName(run.current_step);
