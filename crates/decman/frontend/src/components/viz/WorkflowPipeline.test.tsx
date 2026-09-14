import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";

import { WorkflowPipeline } from "./ApprovalViz";
import { WORKFLOW_STEPS } from "../../workflowSteps";

// The project runs vitest without `globals`, so RTL's automatic cleanup is off
// and MUI's portalled tooltips would leak into the next test.
afterEach(cleanup);

const STEPS = WORKFLOW_STEPS.AddParty;

/** The step dots, in order. Dots and connectors alternate in the DOM. */
const dotsOf = (container: HTMLElement): HTMLElement[] =>
  Array.from(container.querySelectorAll<HTMLElement>("div"))
    .filter((el) => el.querySelector("div") === null)
    .filter((_, i) => i % 2 === 0);

const hoverDot = async (container: HTMLElement, i: number): Promise<string> => {
  fireEvent.mouseOver(dotsOf(container)[i]);
  await waitFor(() => screen.getByRole("tooltip"));
  return screen.getByRole("tooltip").textContent ?? "";
};

describe("WorkflowPipeline", () => {
  it("reveals the current step behind its dot on hover", async () => {
    const current = STEPS.findIndex((s) => s.name === "SyncAcs");
    const { container } = render(
      <WorkflowPipeline current={current} total={STEPS.length} steps={STEPS} />,
    );
    expect(dotsOf(container)).toHaveLength(STEPS.length);

    const tip = await hoverDot(container, current);
    expect(tip).toContain("Copying contracts");
    expect(tip).toContain("imports the party's active contracts");
    expect(tip).toContain(`Step ${current + 1} of ${STEPS.length} · in progress`);
  });

  it("calls an earlier step done", async () => {
    const { container } = render(
      <WorkflowPipeline current={2} total={STEPS.length} steps={STEPS} />,
    );
    const tip = await hoverDot(container, 0);
    expect(tip).toContain("Waiting for members");
    expect(tip).toContain("· done");
  });

  it("calls a later step pending", async () => {
    const { container } = render(
      <WorkflowPipeline current={2} total={STEPS.length} steps={STEPS} />,
    );
    const tip = await hoverDot(container, STEPS.length - 1);
    expect(tip).toContain("Complete");
    expect(tip).toContain("· pending");
  });

  // MUI opens the tooltip on focus-visible, which jsdom cannot produce; the
  // keyboard path is verified against the running app instead. What the
  // component owes is a dot that takes focus and names itself.
  it("makes a labelled dot focusable and names it for a screen reader", () => {
    const { container } = render(
      <WorkflowPipeline current={2} total={STEPS.length} steps={STEPS} />,
    );
    const dot = dotsOf(container)[6];
    expect(dot.tabIndex).toBe(0);
    expect(dot.getAttribute("aria-label")).toContain("Copying contracts");
    expect(dot.getAttribute("aria-label")).toContain(
      `Step 7 of ${STEPS.length} · pending`,
    );
  });

  it("renders bare dots when the step list is withheld", () => {
    const { container } = render(
      <WorkflowPipeline current={2} total={STEPS.length} steps={null} />,
    );
    expect(dotsOf(container)).toHaveLength(STEPS.length);
    expect(dotsOf(container).every((d) => d.tabIndex === -1)).toBe(true);
    fireEvent.mouseOver(dotsOf(container)[2]);
    expect(screen.queryByRole("tooltip")).toBeNull();
  });
});
