import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { SnackbarProvider } from "../contexts";
import { GovernanceSection } from "./GovernanceSection";

// The self-action that replaces GovernanceRules.disclosureObservers (#165).
// These tests drive the form to a submit and read the body it posts.
const ns = "a".repeat(68);
const self = `gov::${ns}`;
const alice = `alice::${ns}`;
const bob = `bob::${ns}`;

const { authenticatedFetch } = vi.hoisted(() => ({ authenticatedFetch: vi.fn() }));
vi.mock("../api", () => ({ authenticatedFetch }));

const ok = (body: unknown) => Promise.resolve({ ok: true, json: async () => body });

beforeEach(() => {
  authenticatedFetch.mockReset();
  authenticatedFetch.mockImplementation((url: string, init?: RequestInit) => {
    if (url.includes("/governance/confirmations")) {
      return ok({ rules_contract_id: "rules-1", threshold: 2, actions: [], domain_actions: [] });
    }
    if (url.includes("/governance/state")) {
      return ok({
        state: {
          contract_id: "rules-1",
          governance_party: self,
          members: [alice],
          threshold: 1,
          additional_proposers: [],
          disclosure_observers: [alice],
          out_of_date: false,
        },
      });
    }
    if (url.includes("/governance/confirm") && init?.method === "POST") {
      return ok({});
    }
    // Every other fetch stays pending, so only the form is under test.
    return new Promise(() => {});
  });
});

const renderForm = () =>
  render(
    <SnackbarProvider>
      <GovernanceSection partyId={self} rulesContractId="rules-1" governanceType="core_self" view="actions" />
    </SnackbarProvider>,
  );

// The Action Type select is the first combobox of the actions view.
const chooseSetObservers = async () => {
  fireEvent.mouseDown((await screen.findAllByRole("combobox", {}, { timeout: 5000 }))[0]);
  fireEvent.click(
    within(screen.getByRole("listbox")).getByRole("option", { name: "Set Disclosure Observers" }),
  );
};

const observersField = () => screen.getByRole("textbox", { name: /^Disclosure Observers/ });

const submit = () => fireEvent.click(screen.getByRole("button", { name: /Submit Confirmation/ }));

// `/governance/confirmations` is a GET, so a POST picks out the confirm.
const confirmPosts = () =>
  authenticatedFetch.mock.calls.filter(
    ([url, init]) => String(url).includes("/governance/confirm") && init?.method === "POST",
  );

const postedConfirm = async () => {
  await waitFor(() => expect(confirmPosts()).toHaveLength(1));
  return JSON.parse(String(confirmPosts()[0][1]?.body));
};

// GovernanceSection is large, so give each render room on a loaded CI runner.
describe("disclosure observers self-action", { timeout: 20_000 }, () => {
  it("starts from the current observers and posts the replacement list", async () => {
    renderForm();
    await chooseSetObservers();
    await waitFor(() => expect(observersField()).toHaveProperty("value", alice));

    fireEvent.change(observersField(), { target: { value: `${alice}\n${bob}` } });
    submit();

    const request = await postedConfirm();
    expect(request.governance_type).toBe("core_self");
    expect(request.action).toEqual({
      type: "governance_set_disclosure_observers",
      disclosure_observers: [alice, bob],
    });
  });

  // The ledger stores the set sorted. A pending vote is matched to its audit
  // entry by the action, so the posted list must be sorted too.
  it("posts the observers in sorted order, whatever order they were typed in", async () => {
    renderForm();
    await chooseSetObservers();
    await waitFor(() => expect(observersField()).toHaveProperty("value", alice));

    fireEvent.change(observersField(), { target: { value: `${bob}\n${alice}` } });
    submit();

    const request = await postedConfirm();
    expect(request.action.disclosure_observers).toEqual([alice, bob]);
  });

  it("sends no list, which the ledger reads as None, when the field is empty", async () => {
    renderForm();
    await chooseSetObservers();
    await waitFor(() => expect(observersField()).toHaveProperty("value", alice));

    fireEvent.change(observersField(), { target: { value: "" } });
    submit();

    const request = await postedConfirm();
    expect(request.action).toEqual({ type: "governance_set_disclosure_observers" });
  });

  it("refuses a party listed twice, without posting", async () => {
    renderForm();
    await chooseSetObservers();
    fireEvent.change(observersField(), { target: { value: `${bob}\n${bob}` } });
    submit();

    expect(await screen.findByText(/listed more than once/)).toBeTruthy();
    expect(confirmPosts()).toHaveLength(0);
  });
});
