import { describe, expect, it } from "vitest";

import type { MemberVariant, WorkflowKind, WorkflowRole } from "./types";
import {
  WORKFLOW_STEPS,
  currentStepLabel,
  humanizeEnumName,
  stepsForRun,
  workflowKindLabel,
} from "./workflowSteps";

const KINDS = Object.keys(WORKFLOW_STEPS) as WorkflowKind[];

/** Every list the table holds, labelled by the run shape that walks it. */
const ALL_LISTS = KINDS.flatMap((kind) => {
  const byRole = WORKFLOW_STEPS[kind];
  const lists: {
    kind: WorkflowKind;
    role: WorkflowRole;
    variant: MemberVariant | null;
    steps: typeof byRole.Coordinator;
  }[] = [
    { kind, role: "Coordinator", variant: null, steps: byRole.Coordinator },
    { kind, role: "Peer", variant: null, steps: byRole.Member },
  ];
  if (byRole.Joiner) {
    lists.push({ kind, role: "Peer", variant: "Joiner", steps: byRole.Joiner });
  }
  return lists;
});

const run = (over: {
  kind: WorkflowKind;
  role: WorkflowRole;
  member_variant?: MemberVariant | null;
  current_step: string;
  step_index: number;
  step_total: number;
}) => over;

/** Shorthand for a coordinator run sitting on `step` of its own list. */
const coordinatorAt = (kind: WorkflowKind, step: string) => {
  const steps = WORKFLOW_STEPS[kind].Coordinator;
  return run({
    kind,
    role: "Coordinator",
    current_step: step,
    step_index: steps.findIndex((s) => s.name === step),
    step_total: steps.length,
  });
};

describe("WORKFLOW_STEPS", () => {
  it("ends every list on Complete", () => {
    for (const { kind, role, variant, steps } of ALL_LISTS) {
      const at = `${kind}/${role}/${variant ?? "default"}`;
      expect(steps.length, at).toBeGreaterThan(1);
      expect(steps[steps.length - 1].name, at).toBe("Complete");
    }
  });

  it("starts every coordinator list on a wait or a key step", () => {
    // The coordinator either waits for acceptances first, or generates its own
    // key before the proposal exists (design D6).
    for (const kind of KINDS) {
      const first = WORKFLOW_STEPS[kind].Coordinator[0].name;
      expect(["WaitingForAcceptances", "GenerateKeys"], kind).toContain(first);
    }
  });

  it("never puts WaitingForAcceptances on a member list", () => {
    // Only the coordinator counts acceptances; a member acts on its own steps.
    for (const { kind, role, variant, steps } of ALL_LISTS) {
      if (role === "Coordinator") continue;
      expect(
        steps.map((s) => s.name),
        `${kind}/${variant ?? "default"}`,
      ).not.toContain("WaitingForAcceptances");
    }
  });

  it("names each step once per list, and labels every step", () => {
    for (const { kind, role, variant, steps } of ALL_LISTS) {
      const at = `${kind}/${role}/${variant ?? "default"}`;
      expect(new Set(steps.map((s) => s.name)).size, at).toBe(steps.length);
      for (const step of steps) {
        expect(step.label, `${at}/${step.name}`).not.toBe("");
        expect(step.description, `${at}/${step.name}`).not.toBe("");
        // A compound name like `SyncAcs` must not reach the UI as-is; a
        // single-word one like `Complete` is already its own label.
        if (/[a-z][A-Z]/.test(step.name)) {
          expect(step.label, `${at}/${step.name}`).not.toBe(step.name);
        }
      }
    }
  });
});

describe("stepsForRun", () => {
  it("returns the list when the run agrees with it", () => {
    const steps = WORKFLOW_STEPS.AddParty.Coordinator;
    expect(stepsForRun(coordinatorAt("AddParty", "AwaitReplication"))).toEqual(steps);
  });

  it("picks the member list for a peer row", () => {
    const steps = WORKFLOW_STEPS.Kick.Member;
    expect(
      stepsForRun(
        run({
          kind: "Kick",
          role: "Peer",
          current_step: "CoSignChanges",
          step_index: 0,
          step_total: steps.length,
        }),
      ),
    ).toEqual(steps);
  });

  it("picks the joiner list for the participant being added", () => {
    const joiner = WORKFLOW_STEPS.AddParty.Joiner;
    expect(joiner).toBeDefined();
    expect(
      stepsForRun(
        run({
          kind: "AddParty",
          role: "Peer",
          member_variant: "Joiner",
          current_step: "SyncAcs",
          step_index: joiner!.findIndex((s) => s.name === "SyncAcs"),
          step_total: joiner!.length,
        }),
      ),
    ).toEqual(joiner);
  });

  it("does not mix a joiner run up with a plain member run", () => {
    // The two lists differ in length, so the total alone already rejects it.
    const member = WORKFLOW_STEPS.AddParty.Member;
    expect(
      stepsForRun(
        run({
          kind: "AddParty",
          role: "Peer",
          member_variant: "Member",
          current_step: "SyncAcs",
          step_index: 2,
          step_total: member.length,
        }),
      ),
    ).toBeNull();
  });

  it("puts the synthetic step on the dot a legacy peer row sits on", () => {
    const steps = WORKFLOW_STEPS.Kick.Member;
    const got = stepsForRun(
      run({
        kind: "Kick",
        role: "Peer",
        current_step: "Active",
        step_index: 0,
        step_total: steps.length,
      }),
    );
    expect(got?.[0].name).toBe("Active");
    expect(got?.[0].label).toBe("Waiting for the coordinator");
    expect(got?.slice(1)).toEqual(steps.slice(1));
  });

  it("drops the list when the backend reports a different step count", () => {
    expect(
      stepsForRun(
        run({
          kind: "Onboarding",
          role: "Coordinator",
          current_step: "ProposeNamespace",
          step_index: 2,
          step_total: WORKFLOW_STEPS.Onboarding.Coordinator.length + 1,
        }),
      ),
    ).toBeNull();
  });

  it("drops the list when the backend reports a step it does not know", () => {
    // A step renamed or replaced on the backend keeps the total, so the name
    // at `step_index` is the only thing that catches it.
    expect(
      stepsForRun(
        run({
          kind: "Onboarding",
          role: "Coordinator",
          current_step: "SignNamespace",
          step_index: 2,
          step_total: WORKFLOW_STEPS.Onboarding.Coordinator.length,
        }),
      ),
    ).toBeNull();
  });

  it("drops the list when step_index points past the end", () => {
    const steps = WORKFLOW_STEPS.Dars.Coordinator;
    expect(
      stepsForRun(
        run({
          kind: "Dars",
          role: "Coordinator",
          current_step: "Complete",
          step_index: steps.length,
          step_total: steps.length,
        }),
      ),
    ).toBeNull();
  });

  it("drops the list when a synthetic step carries an out-of-range index", () => {
    // The synthetic branch replaces the entry at step_index; an index past the
    // end replaces nothing, so it must be rejected like the named path.
    const steps = WORKFLOW_STEPS.Kick.Member;
    expect(
      stepsForRun(
        run({
          kind: "Kick",
          role: "Peer",
          current_step: "Active",
          step_index: steps.length,
          step_total: steps.length,
        }),
      ),
    ).toBeNull();
  });

  it("does not treat an inherited Object key as a synthetic step", () => {
    expect(
      stepsForRun(
        run({
          kind: "Dars",
          role: "Peer",
          current_step: "constructor",
          step_index: 0,
          step_total: WORKFLOW_STEPS.Dars.Member.length,
        }),
      ),
    ).toBeNull();
  });

  it("drops the list when a known step sits at a different index", () => {
    const steps = WORKFLOW_STEPS.Onboarding.Coordinator;
    const at = steps.findIndex((s) => s.name === "ProposeNamespace");
    expect(
      stepsForRun(
        run({
          kind: "Onboarding",
          role: "Coordinator",
          current_step: "ProposeNamespace",
          step_index: at + 1,
          step_total: steps.length,
        }),
      ),
    ).toBeNull();
  });
});

describe("synthetic steps", () => {
  it("keeps the footer and the dot saying the same thing", () => {
    const r = run({
      kind: "Kick",
      role: "Peer",
      current_step: "Active",
      step_index: 0,
      step_total: WORKFLOW_STEPS.Kick.Member.length,
    });
    const onDot = stepsForRun(r)?.[r.step_index];
    expect(currentStepLabel(r)).toBe(onDot?.label);
  });
});

describe("currentStepLabel", () => {
  it("uses the curated label", () => {
    expect(currentStepLabel(coordinatorAt("AddParty", "AwaitReplication"))).toBe(
      "Copying contracts",
    );
  });

  it("names the synthetic step a legacy peer row sits on", () => {
    expect(
      currentStepLabel(
        run({
          kind: "Dars",
          role: "Peer",
          current_step: "Active",
          step_index: 0,
          step_total: WORKFLOW_STEPS.Dars.Member.length,
        }),
      ),
    ).toBe("Waiting for the coordinator");
  });

  it("falls back when the step list is stale", () => {
    expect(
      currentStepLabel(
        run({
          kind: "Kick",
          role: "Coordinator",
          current_step: "SubmitKick",
          step_index: 0,
          step_total: WORKFLOW_STEPS.Kick.Coordinator.length,
        }),
      ),
    ).toBe("Submit kick");
  });
});

describe("humanizeEnumName", () => {
  it("splits PascalCase and keeps acronyms", () => {
    expect(humanizeEnumName("SubmitClearOnboarding")).toBe(
      "Submit clear onboarding",
    );
    expect(humanizeEnumName("SyncAcs")).toBe("Sync acs");
    expect(humanizeEnumName("SubmitDNS")).toBe("Submit DNS");
    expect(humanizeEnumName("Complete")).toBe("Complete");
  });
});

describe("workflowKindLabel", () => {
  it("labels every kind without PascalCase run-together words", () => {
    expect(workflowKindLabel("AddParty")).toBe("Add party");
    expect(workflowKindLabel("ChangeThreshold")).toBe("Change threshold");
    expect(workflowKindLabel("Dars")).toBe("DARs");
    for (const kind of KINDS) {
      expect(workflowKindLabel(kind), kind).not.toBe("");
    }
  });
});
