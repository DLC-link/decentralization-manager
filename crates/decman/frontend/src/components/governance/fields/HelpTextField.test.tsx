import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { HelpTextField } from "./HelpTextField";

describe("HelpTextField", () => {
  it("names the help icon after the label", () => {
    render(<HelpTextField label="Provider Party" value="" help="Who provides it." />);

    expect(screen.getByRole("textbox", { name: /^Provider Party/ })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Help for Provider Party" })).toBeTruthy();
  });

  it("uses helpLabel in the icon name when given", () => {
    render(
      <HelpTextField
        label="Timeout (microseconds)"
        helpLabel="Timeout"
        value=""
        help="How long it lasts."
      />,
    );

    expect(screen.getByRole("button", { name: "Help for Timeout" })).toBeTruthy();
  });

  it("keeps the other slot props", () => {
    render(
      <HelpTextField
        label="Amount"
        type="number"
        value=""
        help="How much."
        slotProps={{ htmlInput: { min: 0, step: "any" } }}
      />,
    );

    const input = screen.getByRole("spinbutton", { name: /^Amount/ });
    expect(input.getAttribute("min")).toBe("0");
    expect(input.getAttribute("step")).toBe("any");
    expect(screen.getByRole("button", { name: "Help for Amount" })).toBeTruthy();
  });
});
