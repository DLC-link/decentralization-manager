import { fireEvent, render, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { FieldDefinition } from "../../types";
import { FieldEditor } from "./FieldEditor";

const renderOptional = (inner: FieldDefinition, label?: string) => {
  const onChange = vi.fn();
  render(
    <FieldEditor
      field={{ type: "optional", inner }}
      onChange={onChange}
      onDelete={() => {}}
      participantCount={4}
      lockStructure
      label={label}
    />,
  );
  return onChange;
};

describe("FieldEditor optional field", () => {
  it("edits an inner proposal timeout", () => {
    const onChange = renderOptional({ type: "rel_time", microseconds: 0 });

    fireEvent.mouseDown(screen.getByRole("combobox"));
    fireEvent.click(within(screen.getByRole("listbox")).getByRole("option", { name: "10 min" }));

    expect(onChange).toHaveBeenCalledWith({
      type: "optional",
      inner: { type: "rel_time", microseconds: 600000000 },
    });
  });

  it("edits an inner threshold, falling back to the default", () => {
    const onChange = renderOptional({ type: "governance_threshold" });

    const input = screen.getByRole("spinbutton", { name: /^Value/ });
    expect((input as HTMLInputElement).value).toBe("3");
    expect(
      screen.getByRole("button", { name: "Help for Optional Governance Threshold" }),
    ).toBeTruthy();
    fireEvent.change(input, { target: { value: "5" } });
    fireEvent.change(input, { target: { value: "" } });

    expect(onChange.mock.calls).toEqual([
      [{ type: "optional", inner: { type: "governance_threshold", value: 5 } }],
      [{ type: "optional", inner: { type: "governance_threshold", value: 3 } }],
    ]);
  });

  it("edits an inner text", () => {
    const onChange = renderOptional({ type: "text", value: "a" });

    expect(screen.getByRole("button", { name: "Help for Optional Text" })).toBeTruthy();
    fireEvent.change(screen.getByRole("textbox", { name: /^Value/ }), {
      target: { value: "abc" },
    });

    expect(onChange).toHaveBeenCalledWith({
      type: "optional",
      inner: { type: "text", value: "abc" },
    });
  });

  it("edits an inner integer, falling back to zero", () => {
    const onChange = renderOptional({ type: "int64", value: 7 });

    const input = screen.getByRole("spinbutton", { name: /^Value/ });
    expect(screen.getByRole("button", { name: "Help for Optional Integer" })).toBeTruthy();
    fireEvent.change(input, { target: { value: "42" } });
    fireEvent.change(input, { target: { value: "x" } });

    expect(onChange.mock.calls).toEqual([
      [{ type: "optional", inner: { type: "int64", value: 42 } }],
      [{ type: "optional", inner: { type: "int64", value: 0 } }],
    ]);
  });

  it("adds and removes inner parties, worded for the observers list", () => {
    const onChange = renderOptional(
      { type: "party_set", parties: ["p1::1220"] },
      "Disclosure Observers",
    );

    const input = screen.getByPlaceholderText(
      "Paste party ID, press Enter or click away (leave empty for no observers)",
    );
    expect(screen.getByRole("button", { name: "Help for Disclosure Observers" })).toBeTruthy();
    fireEvent.change(input, { target: { value: "p2::1220" } });
    fireEvent.blur(input);
    fireEvent.click(screen.getAllByRole("button").at(-1)!);

    expect(onChange.mock.calls).toEqual([
      [{ type: "optional", inner: { type: "party_set", parties: ["p1::1220", "p2::1220"] } }],
      [{ type: "optional", inner: { type: "party_set", parties: [] } }],
    ]);
  });

  it("words an inner party set for extra proposers by default", () => {
    renderOptional({ type: "party_set", parties: [] }, "Additional Proposers");

    expect(
      screen.getByPlaceholderText(
        "Paste party ID, press Enter or click away (leave empty for no extra proposers)",
      ),
    ).toBeTruthy();
    expect(screen.getByRole("button", { name: "Help for Additional Proposers" })).toBeTruthy();
  });

  it("shows no value input for the other inner types", () => {
    renderOptional({ type: "bool", value: true });

    expect(screen.queryByRole("textbox")).toBeNull();
    expect(screen.queryByRole("spinbutton")).toBeNull();
    expect(screen.queryByRole("combobox")).toBeNull();
  });
});
