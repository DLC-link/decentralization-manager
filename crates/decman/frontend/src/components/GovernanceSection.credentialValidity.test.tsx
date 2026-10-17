import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { SnackbarProvider } from "../contexts";
import { GovernanceSection } from "./GovernanceSection";

// The three proposals that mint credentials carry an optional validity
// window (#346). These tests drive the Onboard Instrument Issuers form to a
// submit and read the request body the form posts.
const ns = "a".repeat(68);
const self = `gov::${ns}`;
const issuer = `issuer::${ns}`;

const { authenticatedFetch } = vi.hoisted(() => ({ authenticatedFetch: vi.fn() }));
vi.mock("../api", () => ({ authenticatedFetch }));

const ok = (body: unknown) => Promise.resolve({ ok: true, json: async () => body });

beforeEach(() => {
  authenticatedFetch.mockReset();
  authenticatedFetch.mockImplementation((url: string, init?: RequestInit) => {
    if (url.includes("/governance/confirmations")) {
      return ok({ rules_contract_id: "rules-1", threshold: 2, domain_actions: [] });
    }
    if (url.includes("/instruments")) {
      return ok({
        instruments: [{ contract_id: "00icc", instrument_admin: self, instrument_id: "TOKEN" }],
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

// The InstrumentConfiguration select's label is not linked either, so find
// it through its FormControl.
const chooseInstrument = async () => {
  const label = screen.getAllByText("InstrumentConfiguration").find((el) => el.closest("label"));
  const control = label?.closest(".MuiFormControl-root") as HTMLElement;
  fireEvent.mouseDown(within(control).getByRole("combobox"));
  fireEvent.click(
    await within(screen.getByRole("listbox")).findByRole("option", { name: /^TOKEN/ }),
  );
};

const fillIssuersForm = async () => {
  await chooseType("Onboard Instrument Issuers (as Registrar)");
  await chooseInstrument();
  fireEvent.change(screen.getByRole("textbox", { name: /^Instrument Issuers/ }), {
    target: { value: issuer },
  });
};

const setWindow = (from: string, until: string) => {
  fireEvent.change(screen.getByLabelText(/^Credentials Valid From/), { target: { value: from } });
  fireEvent.change(screen.getByLabelText(/^Credentials Valid Until/), { target: { value: until } });
};

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

// datetime-local values are local time, so expect what the browser parses.
const micros = (local: string) => new Date(local).getTime() * 1000;

// GovernanceSection is large, so give each render room on a loaded CI runner.
describe("credential validity window", { timeout: 20_000 }, () => {
  it("sends the window as micros since epoch", async () => {
    renderForm();
    await fillIssuersForm();
    setWindow("2030-01-01T09:00", "2030-07-01T09:00");
    submit();

    const proposal = await postedProposal();
    expect(proposal).toMatchObject({
      type: "onboard_instrument_issuers",
      instrument_configuration_cid: "00icc",
      instrument_issuers: [issuer],
      credential_valid_from_micros: micros("2030-01-01T09:00"),
      credential_valid_until_micros: micros("2030-07-01T09:00"),
    });
  });

  it("sends no window when both fields are empty", async () => {
    renderForm();
    await fillIssuersForm();
    submit();

    const proposal = await postedProposal();
    expect(proposal).not.toHaveProperty("credential_valid_from_micros");
    expect(proposal).not.toHaveProperty("credential_valid_until_micros");
  });

  it("refuses a window that ends before it starts, without posting", async () => {
    renderForm();
    await fillIssuersForm();
    setWindow("2030-07-01T09:00", "2030-01-01T09:00");
    submit();

    expect(await screen.findByText(/must be before Credentials Valid Until/)).toBeTruthy();
    expect(
      authenticatedFetch.mock.calls.some(([url]) => String(url).includes("/governance/propose")),
    ).toBe(false);
  });

  it("shows the window only on the minting proposals and clears it on a type change", async () => {
    renderForm();
    await chooseType("Onboard Instrument Issuers (as Registrar)");
    fireEvent.change(screen.getByLabelText(/^Credentials Valid Until/), {
      target: { value: "2030-07-01T09:00" },
    });

    await chooseType("Generic Vote");
    expect(screen.queryByLabelText(/^Credentials Valid Until/)).toBeNull();

    await chooseType("6. Provision Instrument (as Registrar)");
    expect(screen.getByLabelText(/^Credentials Valid Until/)).toHaveProperty("value", "");
  });
});
