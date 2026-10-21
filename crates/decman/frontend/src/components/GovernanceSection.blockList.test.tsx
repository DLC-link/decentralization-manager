import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { SnackbarProvider } from "../contexts";
import { GovernanceSection } from "./GovernanceSection";

// Setup Utility and Provision Instrument carry an optional block list flag
// (#396). These tests drive each form to a submit and read the request body
// the form posts.
const ns = "a".repeat(68);
const self = `gov::${ns}`;

const { authenticatedFetch } = vi.hoisted(() => ({ authenticatedFetch: vi.fn() }));
vi.mock("../api", () => ({ authenticatedFetch }));

const ok = (body: unknown) => Promise.resolve({ ok: true, json: async () => body });

beforeEach(() => {
  authenticatedFetch.mockReset();
  authenticatedFetch.mockImplementation((url: string, init?: RequestInit) => {
    if (url.includes("/governance/confirmations")) {
      return ok({ rules_contract_id: "rules-1", threshold: 2, domain_actions: [] });
    }
    if (url.includes("/governance/propose") && init?.method === "POST") {
      return ok({});
    }
    // Every other fetch stays pending, so only the form is under test.
    return new Promise(() => {});
  });
});

const renderForm = () =>
  render(
    <SnackbarProvider>
      <GovernanceSection partyId={self} rulesContractId="rules-1" view="proposals" />
    </SnackbarProvider>,
  );

// The Proposal Type select has no accessible name (its label is not linked),
// and it sits above every form's own fields, so it is the first combobox.
const chooseType = async (option: string) => {
  fireEvent.mouseDown((await screen.findAllByRole("combobox", {}, { timeout: 5000 }))[0]);
  fireEvent.click(within(screen.getByRole("listbox")).getByRole("option", { name: option }));
};

// The page behind a just-closed Select menu stays aria-hidden in jsdom, so
// find the checkbox through its label rather than its role.
const blockListBox = () => screen.getByLabelText(/^Use block list/);

const submit = () =>
  fireEvent.click(screen.getAllByRole("button", { name: /Submit Proposal/ })[0]);

const postedProposal = async () => {
  await waitFor(() =>
    expect(authenticatedFetch).toHaveBeenCalledWith(
      expect.stringContaining("/governance/propose"),
      expect.objectContaining({ method: "POST" }),
    ),
  );
  const call = authenticatedFetch.mock.calls.find(([url]) =>
    String(url).includes("/governance/propose"),
  );
  return JSON.parse(String(call?.[1]?.body)).proposal;
};

const forms = [
  { option: "3. Setup Utility", type: "setup_utility" },
  { option: "6. Provision Instrument (as Registrar)", type: "provision_instrument" },
];

// GovernanceSection is large, so give each render room on a loaded CI runner.
describe("use block list", { timeout: 20_000 }, () => {
  for (const { option, type } of forms) {
    it(`${type}: sends use_block_list true when checked`, async () => {
      renderForm();
      await chooseType(option);
      fireEvent.click(blockListBox());
      submit();

      expect(await postedProposal()).toMatchObject({ type, use_block_list: true });
    });

    it(`${type}: sends no use_block_list when unchecked`, async () => {
      renderForm();
      await chooseType(option);
      expect(blockListBox()).toHaveProperty("checked", false);
      submit();

      const proposal = await postedProposal();
      expect(proposal.type).toBe(type);
      expect(proposal).not.toHaveProperty("use_block_list");
    });
  }

  it("clears the flag on a type change", async () => {
    renderForm();
    await chooseType("3. Setup Utility");
    fireEvent.click(blockListBox());
    expect(blockListBox()).toHaveProperty("checked", true);

    await chooseType("Generic Vote");
    expect(screen.queryByLabelText(/^Use block list/)).toBeNull();

    await chooseType("6. Provision Instrument (as Registrar)");
    expect(blockListBox()).toHaveProperty("checked", false);
  });
});
