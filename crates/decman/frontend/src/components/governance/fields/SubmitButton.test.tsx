import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { SubmitButton } from "./SubmitButton";

describe("SubmitButton", () => {
  it("renders a contained button with a check icon in place", () => {
    const onClick = vi.fn();
    const { container } = render(
      <SubmitButton label="Submit Proposal" loading={false} disabled={false} onClick={onClick} />,
    );

    const button = screen.getByRole("button", { name: "Submit Proposal" });
    expect(button.className).toContain("MuiButton-contained");
    expect(button.querySelector('[data-testid="CheckCircleIcon"]')).not.toBeNull();
    expect(button.parentElement).toBe(container);
    fireEvent.click(button);
    expect(onClick).toHaveBeenCalledTimes(1);
  });

  it("shows a spinner while loading and honours disabled", () => {
    render(<SubmitButton label="Submit Proposal" loading disabled onClick={() => {}} />);

    const button = screen.getByRole("button", { name: "Submit Proposal" });
    expect(button.hasAttribute("disabled")).toBe(true);
    expect(screen.getByRole("progressbar")).toBeTruthy();
    expect(button.querySelector('[data-testid="CheckCircleIcon"]')).toBeNull();
  });

  it("wraps the in-place button in a Box when inlineBoxSx is set", () => {
    const { container } = render(
      <SubmitButton
        label="Submit Confirmation"
        loading={false}
        disabled={false}
        onClick={() => {}}
        inlineBoxSx={{ mt: 2 }}
      />,
    );

    const button = screen.getByRole("button", { name: "Submit Confirmation" });
    expect(button.parentElement?.parentElement).toBe(container);
    expect(button.parentElement?.className).toContain("MuiBox-root");
  });

  it("renders a plain button, without icon or Box, into the portal node", () => {
    const portal = document.createElement("div");
    document.body.appendChild(portal);
    const { container } = render(
      <SubmitButton
        label="Submit Confirmation"
        loading={false}
        disabled={false}
        onClick={() => {}}
        portalEl={portal}
        inlineBoxSx={{ mt: 2 }}
      />,
    );

    expect(container.innerHTML).toBe("");
    const button = screen.getByRole("button", { name: "Submit Confirmation" });
    expect(button.parentElement).toBe(portal);
    expect(button.className).not.toContain("MuiButton-contained");
    expect(button.querySelector("svg")).toBeNull();
    portal.remove();
  });
});
