import { describe, expect, it } from "vitest";

import type { WorkflowKind } from "./types";
import {
  WORKFLOW_STEPS,
  currentStepLabel,
  humanizeEnumName,
  stepsForRun,
  workflowKindLabel,
} from "./workflowSteps";

const KINDS = Object.keys(WORKFLOW_STEPS) as WorkflowKind[];

const run = (over: {
  kind: WorkflowKind;
  current_step: string;
  step_index: number;
  step_total: number;
}) => over;

describe("WORKFLOW_STEPS", () => {
  it("starts every kind on WaitingForPeers and ends on Complete", () => {
    for (const kind of KINDS) {
      const steps = WORKFLOW_STEPS[kind];
      expect(steps.length, kind).toBeGreaterThan(1);
      expect(steps[0].name, kind).toBe("WaitingForPeers");
      expect(steps[steps.length - 1].name, kind).toBe("Complete");
    }
  });

  it("names each step once, and labels every step", () => {
    for (const kind of KINDS) {
      const steps = WORKFLOW_STEPS[kind];
      expect(new Set(steps.map((s) => s.name)).size, kind).toBe(steps.length);
      for (const step of steps) {
        expect(step.label, `${kind}/${step.name}`).not.toBe("");
        expect(step.description, `${kind}/${step.name}`).not.toBe("");
        // A compound name like `SyncAcs` must not reach the UI as-is; a
        // single-word one like `Complete` is already its own label.
        if (/[a-z][A-Z]/.test(step.name)) {
          expect(step.label, `${kind}/${step.name}`).not.toBe(step.name);
        }
      }
    }
  });
});

describe("stepsForRun", () => {
  it("returns the list when the run agrees with it", () => {
    const steps = WORKFLOW_STEPS.AddParty;
    const at = steps.findIndex((s) => s.name === "SyncAcs");
    expect(
      stepsForRun(
        run({
          kind: "AddParty",
          current_step: "SyncAcs",
          step_index: at,
          step_total: steps.length,
        }),
      ),
    ).toEqual(steps);
  });

  it("puts the synthetic step on the dot the peer row sits on", () => {
    const steps = WORKFLOW_STEPS.Kick;
    const got = stepsForRun(
      run({
        kind: "Kick",
        current_step: "Active",
        step_index: 0,
        step_total: steps.length,
      }),
    );
    // The peer is not waiting for members — it waits for the coordinator — so
    // the dot it sits on must not describe WaitingForPeers.
    expect(got?.[0].name).toBe("Active");
    expect(got?.[0].label).toBe("Waiting for the coordinator");
    expect(got?.slice(1)).toEqual(steps.slice(1));
  });

  it("drops the list when the backend reports a different step count", () => {
    expect(
      stepsForRun(
        run({
          kind: "Onboarding",
          current_step: "SignDns",
          step_index: 3,
          step_total: WORKFLOW_STEPS.Onboarding.length + 1,
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
          current_step: "SignNamespace",
          step_index: 3,
          step_total: WORKFLOW_STEPS.Onboarding.length,
        }),
      ),
    ).toBeNull();
  });

  it("drops the list when step_index points past the end", () => {
    expect(
      stepsForRun(
        run({
          kind: "Dars",
          current_step: "Complete",
          step_index: WORKFLOW_STEPS.Dars.length,
          step_total: WORKFLOW_STEPS.Dars.length,
        }),
      ),
    ).toBeNull();
  });

  it("drops the list when a synthetic step carries an out-of-range index", () => {
    // The synthetic branch replaces the entry at step_index; an index past the
    // end replaces nothing, so it must be rejected like the enum path.
    expect(
      stepsForRun(
        run({
          kind: "Kick",
          current_step: "Active",
          step_index: WORKFLOW_STEPS.Kick.length,
          step_total: WORKFLOW_STEPS.Kick.length,
        }),
      ),
    ).toBeNull();
  });

  it("does not treat an inherited Object key as a synthetic step", () => {
    expect(
      stepsForRun(
        run({
          kind: "Dars",
          current_step: "constructor",
          step_index: 0,
          step_total: WORKFLOW_STEPS.Dars.length,
        }),
      ),
    ).toBeNull();
  });

  it("drops the list when a known step sits at a different index", () => {
    const steps = WORKFLOW_STEPS.Onboarding;
    const at = steps.findIndex((s) => s.name === "SignDns");
    expect(
      stepsForRun(
        run({
          kind: "Onboarding",
          current_step: "SignDns",
          step_index: at + 1,
          step_total: steps.length,
        }),
      ),
    ).toBeNull();
  });
});

describe("synthetic steps", () => {
  it("keeps the footer and the dot saying the same thing", () => {
    const r = {
      kind: "Kick" as const,
      current_step: "Active",
      step_index: 0,
      step_total: WORKFLOW_STEPS.Kick.length,
    };
    const onDot = stepsForRun(r)?.[r.step_index];
    expect(currentStepLabel(r)).toBe(onDot?.label);
  });
});

describe("currentStepLabel", () => {
  it("uses the curated label", () => {
    const steps = WORKFLOW_STEPS.AddParty;
    expect(
      currentStepLabel(
        run({
          kind: "AddParty",
          current_step: "SyncAcs",
          step_index: steps.findIndex((s) => s.name === "SyncAcs"),
          step_total: steps.length,
        }),
      ),
    ).toBe("Copying contracts");
  });

  it("names the synthetic step a peer row starts on", () => {
    expect(
      currentStepLabel(
        run({
          kind: "Dars",
          current_step: "Active",
          step_index: 0,
          step_total: WORKFLOW_STEPS.Dars.length,
        }),
      ),
    ).toBe("Waiting for the coordinator");
  });

  it("falls back when the step list is stale", () => {
    expect(
      currentStepLabel(
        run({
          kind: "Kick",
          current_step: "SubmitKick",
          step_index: 0,
          step_total: WORKFLOW_STEPS.Kick.length,
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
