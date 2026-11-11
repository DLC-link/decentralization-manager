import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { RequirementRows, type RequirementRow } from "./RequirementRows";

const rows: RequirementRow[] = [
  { issuer: "alice::1220", claimsText: "role,registrar" },
  { issuer: "bob::1220", claimsText: "" },
];

const renderRows = (setRows = vi.fn()) => {
  render(
    <RequirementRows
      label="Holder Requirements"
      help="What a holder needs."
      rows={rows}
      setRows={setRows}
      defaultIssuer="gov::1220"
    />,
  );
  return setRows;
};

describe("RequirementRows", () => {
  it("shows the list label and one issuer and claims field per row", () => {
    renderRows();

    expect(screen.getByText("Holder Requirements (add issuer + required claims)")).toBeTruthy();
    const issuers = screen.getAllByRole("textbox", { name: /^Issuer Party/ });
    expect(issuers.map((f) => (f as HTMLInputElement).value)).toEqual([
      "alice::1220",
      "bob::1220",
    ]);
    expect(screen.getAllByRole("textbox", { name: /^Required claims/ })).toHaveLength(2);
  });

  it("edits one row and keeps the others", () => {
    const setRows = renderRows();

    fireEvent.change(screen.getAllByRole("textbox", { name: /^Required claims/ })[1], {
      target: { value: "kyc,true" },
    });

    expect(setRows).toHaveBeenCalledWith([rows[0], { ...rows[1], claimsText: "kyc,true" }]);
  });

  it("removes a row", () => {
    const setRows = renderRows();

    fireEvent.click(screen.getAllByRole("button", { name: "Remove" })[0]);

    expect(setRows).toHaveBeenCalledWith([rows[1]]);
  });

  it("adds a row for the default issuer", () => {
    const setRows = renderRows();

    fireEvent.click(screen.getByRole("button", { name: "Add Requirement" }));

    expect(setRows).toHaveBeenCalledWith([...rows, { issuer: "gov::1220", claimsText: "" }]);
  });
});
