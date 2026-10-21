import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { SnackbarProvider } from "../contexts";
import { GovernanceSection } from "./GovernanceSection";

// Update Instrument Configuration replaces every field (#398), so the form
// starts from the chosen configuration's current values. These tests drive
// the form to a submit and read the request body it posts.
const ns = "a".repeat(68);
const self = `gov::${ns}`;
const issuer = `issuer::${ns}`;

const readable = {
  contract_id: "00icc",
  instrument_admin: self,
  instrument_id: "TOKEN",
  configuration: {
    additional_identifiers: [{ source: self, id: "TICK", scheme: "Ticker" }],
    issuer_requirements: [
      { issuer: self, required_claims: [{ property: "role", value: "instrument-issuer" }] },
    ],
    holder_requirements: [{ issuer, required_claims: [{ property: "kyc", value: "passed" }] }],
    check_block_list: true,
  },
};

const { authenticatedFetch } = vi.hoisted(() => ({ authenticatedFetch: vi.fn() }));
vi.mock("../api", () => ({ authenticatedFetch }));

const ok = (body: unknown) => Promise.resolve({ ok: true, json: async () => body });

const mockLedger = (instrument: object) => {
  authenticatedFetch.mockReset();
  authenticatedFetch.mockImplementation((url: string, init?: RequestInit) => {
    if (url.includes("/governance/confirmations")) {
      return ok({ rules_contract_id: "rules-1", threshold: 2, domain_actions: [] });
    }
    if (url.includes("/instruments")) {
      return ok({ instruments: [instrument] });
    }
    if (url.includes("/services/registrar")) {
      return ok({ services: [{ contract_id: "00rsc", operator: `op::${ns}`, registrar: self }] });
    }
    if (url.includes("/governance/propose") && init?.method === "POST") {
      return ok({});
    }
    // Every other fetch stays pending, so only the form is under test.
    return new Promise(() => {});
  });
};

beforeEach(() => mockLedger(readable));

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

// The form's selects are not linked to their labels either, so find each
// one through its FormControl.
const choose = async (label: string, option: RegExp) => {
  const labelEl = screen.getAllByText(label).find((el) => el.closest("label"));
  const control = labelEl?.closest(".MuiFormControl-root") as HTMLElement;
  fireEvent.mouseDown(within(control).getByRole("combobox"));
  fireEvent.click(await within(screen.getByRole("listbox")).findByRole("option", { name: option }));
};

const openForm = async () => {
  renderForm();
  await chooseType("Update Instrument Configuration (as Registrar)");
  await choose("RegistrarService", /^00rsc/);
  await choose("InstrumentConfiguration", /^TOKEN/);
};

const submit = () =>
  fireEvent.click(screen.getAllByRole("button", { name: /Submit Proposal/ })[0]);

const proposeCalls = () =>
  authenticatedFetch.mock.calls.filter(([url]) => String(url).includes("/governance/propose"));

const postedProposal = async () => {
  await waitFor(() => expect(proposeCalls()).toHaveLength(1));
  return JSON.parse(String(proposeCalls()[0][1]?.body)).proposal;
};

// GovernanceSection is large, so give each render room on a loaded CI runner.
describe("update instrument configuration", { timeout: 20_000 }, () => {
  it("posts the current values when nothing is changed", async () => {
    await openForm();
    submit();

    expect(await postedProposal()).toEqual({
      type: "update_instrument_configuration",
      registrar_service_cid: "00rsc",
      instrument_configuration_cid: "00icc",
      instrument_id_text: "TOKEN",
      additional_identifiers: [{ source: self, id: "TICK", scheme: "Ticker" }],
      issuer_requirements: [
        { issuer: self, required_claims: [{ property: "role", value: "instrument-issuer" }] },
      ],
      holder_requirements: [{ issuer, required_claims: [{ property: "kyc", value: "passed" }] }],
      use_block_list: true,
    });
  });

  it("sends a cleared field as cleared, and an unchecked flag as null", async () => {
    await openForm();
    fireEvent.click(screen.getByLabelText(/^Use block list/));
    // The first Remove button belongs to the only identifier row.
    fireEvent.click(screen.getAllByRole("button", { name: "Remove", hidden: true })[0]);
    submit();

    const proposal = await postedProposal();
    expect(proposal.additional_identifiers).toEqual([]);
    expect(proposal.use_block_list).toBeNull();
    expect(proposal.issuer_requirements).toHaveLength(1);
  });

  it("refuses an identifier row with an empty field, without posting", async () => {
    await openForm();
    fireEvent.change(screen.getByLabelText("Identifier 1 ID"), { target: { value: "" } });
    submit();

    expect(await screen.findByText(/Additional identifier 1: source and id are required/))
      .toBeTruthy();
    expect(proposeCalls()).toHaveLength(0);
  });

  it("warns when the current values could not be read", async () => {
    mockLedger({ contract_id: "00icc", instrument_admin: self, instrument_id: "TOKEN" });
    await openForm();

    expect(screen.getByText(/could not read this configuration/)).toBeTruthy();
    expect(screen.getByLabelText(/^Instrument ID/)).toHaveProperty("value", "TOKEN");
    expect(screen.queryByLabelText("Identifier 1 ID")).toBeNull();
  });

  // The backend sends a failed configuration read as JSON null.
  it("warns when the configuration read failed and came back null", async () => {
    mockLedger({ ...readable, configuration: null });
    await openForm();

    expect(screen.getByText(/could not read this configuration/)).toBeTruthy();
    expect(screen.queryByLabelText("Identifier 1 ID")).toBeNull();
  });

  // The ledger requires only a nonempty id, so an empty scheme is valid, and
  // an id with edge spaces must not be trimmed into a different identifier.
  it("posts identifiers verbatim, including edge spaces and an empty scheme", async () => {
    const identifiers = [
      { source: self, id: " TICK ", scheme: "" },
      { source: issuer, id: "ISIN-1", scheme: " ISIN " },
    ];
    mockLedger({
      ...readable,
      configuration: { ...readable.configuration, additional_identifiers: identifiers },
    });
    await openForm();
    submit();

    expect((await postedProposal()).additional_identifiers).toEqual(identifiers);
  });

  // The editor's "property,value" lines cannot carry these values, so an
  // unchanged row must send its original claims rather than the parsed text.
  it("posts claims with commas, newlines and edge spaces unchanged", async () => {
    const awkward = [
      { property: "region", value: "EU,US" },
      { property: "status", value: " approved " },
      { property: "note", value: "line one\nline two" },
    ];
    mockLedger({
      ...readable,
      configuration: {
        ...readable.configuration,
        holder_requirements: [{ issuer, required_claims: awkward }],
      },
    });
    await openForm();
    submit();

    const proposal = await postedProposal();
    expect(proposal.holder_requirements).toEqual([{ issuer, required_claims: awkward }]);
    expect(proposal.issuer_requirements).toEqual(readable.configuration.issuer_requirements);
  });
});
