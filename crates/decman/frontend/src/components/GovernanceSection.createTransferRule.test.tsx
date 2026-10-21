import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { SnackbarProvider } from "../contexts";
import { GovernanceSection } from "./GovernanceSection";

// Create Transfer Rule adds a TransferRule to this party's RegistrarService
// (#397). These tests drive the form to a submit and read the request body
// it posts.
const ns = "a".repeat(68);
const self = `gov::${ns}`;
const provider = `provider::${ns}`;

const { authenticatedFetch } = vi.hoisted(() => ({ authenticatedFetch: vi.fn() }));
vi.mock("../api", () => ({ authenticatedFetch }));

const ok = (body: unknown) => Promise.resolve({ ok: true, json: async () => body });

beforeEach(() => {
  authenticatedFetch.mockReset();
  authenticatedFetch.mockImplementation((url: string, init?: RequestInit) => {
    if (url.includes("/governance/confirmations")) {
      return ok({ rules_contract_id: "rules-1", threshold: 2, domain_actions: [] });
    }
    if (url.includes("/services/registrar")) {
      // The second service names another registrar: this party co-signed it
      // as the provider, and the action would refuse it.
      return ok({
        services: [
          { contract_id: "00own", operator: `op::${ns}`, registrar: self },
          { contract_id: "00foreign", operator: `op::${ns}`, registrar: provider },
        ],
      });
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

// The RegistrarService select's label is not linked either, so find it
// through its FormControl.
const openServiceMenu = () => {
  const label = screen.getAllByText("RegistrarService").find((el) => el.closest("label"));
  const control = label?.closest(".MuiFormControl-root") as HTMLElement;
  fireEvent.mouseDown(within(control).getByRole("combobox"));
  return screen.getByRole("listbox");
};

const postedProposal = async () => {
  const proposeCalls = () =>
    authenticatedFetch.mock.calls.filter(([url]) => String(url).includes("/governance/propose"));
  await waitFor(() => expect(proposeCalls()).toHaveLength(1));
  return JSON.parse(String(proposeCalls()[0][1]?.body)).proposal;
};

// GovernanceSection is large, so give each render room on a loaded CI runner.
describe("create transfer rule", { timeout: 20_000 }, () => {
  it("posts the chosen RegistrarService", async () => {
    renderForm();
    await chooseType("Create Transfer Rule");
    fireEvent.click(await within(openServiceMenu()).findByRole("option", { name: "00own" }));
    fireEvent.click(screen.getAllByRole("button", { name: /Submit Proposal/ })[0]);

    expect(await postedProposal()).toEqual({
      type: "create_transfer_rule",
      registrar_service_cid: "00own",
    });
  });

  it("offers only services whose registrar is this party", async () => {
    renderForm();
    await chooseType("Create Transfer Rule");
    const menu = openServiceMenu();

    expect(await within(menu).findByRole("option", { name: "00own" })).toBeTruthy();
    expect(within(menu).queryByRole("option", { name: "00foreign" })).toBeNull();
  });
});
