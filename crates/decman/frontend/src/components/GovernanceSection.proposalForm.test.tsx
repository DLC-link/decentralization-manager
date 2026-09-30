import { fireEvent, render, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { SnackbarProvider } from "../contexts";
import { GovernanceSection } from "./GovernanceSection";

// The proposal form renders once /governance/confirmations answers. Every
// other fetch stays pending, so only the form's own state is under test.
vi.mock("../api", () => ({
  authenticatedFetch: vi.fn((url: string) =>
    url.includes("/governance/confirmations")
      ? Promise.resolve({
          ok: true,
          json: async () => ({ rules_contract_id: "rules-1", threshold: 2, domain_actions: [] }),
        })
      : new Promise(() => {}),
  ),
}));

const ns = "a".repeat(68);
const self = `gov::${ns}`;
const operator = `operator::${ns}`;

const renderForm = () =>
  render(
    <SnackbarProvider>
      <GovernanceSection
        partyId={self}
        rulesContractId="rules-1"
        defaultOperatorParty={operator}
        view="proposals"
      />
    </SnackbarProvider>,
  );

// The Proposal Type select has no accessible name (its label is not linked),
// and it sits above every form's own fields, so it is the first combobox.
const chooseType = async (option: string) => {
  fireEvent.mouseDown((await screen.findAllByRole("combobox", {}, { timeout: 5000 }))[0]);
  fireEvent.click(within(screen.getByRole("listbox")).getByRole("option", { name: option }));
};

const field = (label: string) => screen.getByRole("textbox", { name: new RegExp(`^${label}`) });

// GovernanceSection is large, so give each render room on a loaded CI runner.
describe("proposal form across proposal types", { timeout: 20_000 }, () => {
  it("does not carry this party into the next form's Provider Party", async () => {
    renderForm();
    await chooseType("2. Create Provider Service Request (as Provider)");
    expect(field("Provider Party")).toHaveProperty("value", self);

    await chooseType("4. Create Registrar Service Request (as Registrar)");
    // Here the provider is the other decentralized party, so it starts empty.
    expect(field("Provider Party")).toHaveProperty("value", "");
    // Fields prefilled from app state keep their value.
    expect(field("Operator Party")).toHaveProperty("value", operator);
  });

  it("clears a value typed for one type when switching to another", async () => {
    renderForm();
    await chooseType("4. Create Registrar Service Request (as Registrar)");
    fireEvent.change(field("Provider Party"), { target: { value: `other::${ns}` } });
    fireEvent.change(field("Operator Party"), { target: { value: `typo::${ns}` } });

    await chooseType("2. Create Provider Service Request (as Provider)");
    expect(field("Provider Party")).toHaveProperty("value", self);
    expect(field("Operator Party")).toHaveProperty("value", operator);

    await chooseType("4. Create Registrar Service Request (as Registrar)");
    expect(field("Provider Party")).toHaveProperty("value", "");
  });

  it("keeps the form when the same type is picked again", async () => {
    renderForm();
    await chooseType("4. Create Registrar Service Request (as Registrar)");
    fireEvent.change(field("Provider Party"), { target: { value: `other::${ns}` } });
    await chooseType("4. Create Registrar Service Request (as Registrar)");
    expect(field("Provider Party")).toHaveProperty("value", `other::${ns}`);
  });
});
