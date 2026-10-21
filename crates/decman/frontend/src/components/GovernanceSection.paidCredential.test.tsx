import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { SnackbarProvider } from "../contexts";
import { GovernanceSection } from "./GovernanceSection";

// The Offer Paid Credential and Accept Paid Credential forms (#125). These
// tests drive each form to a submit and read the request body it posts.
const ns = "a".repeat(68);
const self = `gov::${ns}`;
const operator = `operator::${ns}`;
const issuer = `issuer::${ns}`;
const holder = `holder::${ns}`;
const stranger = `stranger::${ns}`;
const ownAmuletCid = "00amulet-own-0001";

const { authenticatedFetch } = vi.hoisted(() => ({ authenticatedFetch: vi.fn() }));
vi.mock("../api", () => ({ authenticatedFetch }));

const ok = (body: unknown) => Promise.resolve({ ok: true, json: async () => body });

const offer = (contract_id: string, is_free: boolean) => ({
  contract_id,
  operator,
  issuer,
  holder: self,
  credential_id: contract_id,
  description: "",
  is_free,
});

const amulet = (contract_id: string, owner: string, initialAmount: string) => ({
  contract_id,
  blob: "",
  payload: { dso: `DSO::${ns}`, owner, amount: { initialAmount } },
});

beforeEach(() => {
  authenticatedFetch.mockReset();
  authenticatedFetch.mockImplementation((url: string, init?: RequestInit) => {
    if (url.includes("/governance/confirmations")) {
      return ok({ rules_contract_id: "rules-1", threshold: 2, domain_actions: [] });
    }
    if (url.includes("/services/user")) {
      return ok({ services: [{ contract_id: "00usc", operator, user: self }] });
    }
    if (url.includes("/services/provider")) {
      return ok({ services: [] });
    }
    if (url.includes("/credential-offers")) {
      return ok({
        credential_offers: [offer("00free-offer", true), offer("00paid-offer", false)],
      });
    }
    if (url.includes("/contracts/query") && url.includes("Splice.Amulet")) {
      return ok({
        contracts: [
          amulet(ownAmuletCid, self, "100.0000000000"),
          amulet("00amulet-stranger", stranger, "500.0000000000"),
        ],
      });
    }
    if (url.includes("/network-info")) {
      return ok({ dso_party_id: `DSO::${ns}`, amulet_rules_cid: "00rules", amulet_rules_blob: "" });
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

const escape = (text: string) => text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
const field = (label: string) =>
  screen.getByRole("textbox", { name: new RegExp(`^${escape(label)}`) });
const type = (label: string, value: string) =>
  fireEvent.change(field(label), { target: { value } });

const submit = () =>
  fireEvent.click(screen.getAllByRole("button", { name: /Submit Proposal/ })[0]);

const proposeCalls = () =>
  authenticatedFetch.mock.calls.filter(([url]) => String(url).includes("/governance/propose"));

const postedProposal = async () => {
  await waitFor(() => expect(proposeCalls()).toHaveLength(1));
  return JSON.parse(String(proposeCalls()[0][1]?.body)).proposal;
};

// The UserService select prefills once /services/user answers.
const waitForUserService = () =>
  waitFor(() => expect(screen.getAllByText("00usc").length).toBeGreaterThan(0));

const fillPaidOffer = async (billing: {
  fee: string;
  period: string;
  target: string;
  weight?: string;
  initial?: string;
}) => {
  await chooseType("Offer Paid Credential");
  await waitForUserService();
  type("Holder Party", holder);
  type("Credential ID", "paid-1");
  type("Description", "Paid test credential");
  type("Fee per Day (USD)", billing.fee);
  type("Billing Period (minutes)", billing.period);
  type("Deposit Target (USD)", billing.target);
  if (billing.weight !== undefined) type("Holder Activity Weight", billing.weight);
  if (billing.initial !== undefined) type("Initial Deposit (USD)", billing.initial);
};

// GovernanceSection is large, so give each render room on a loaded CI runner.
describe("Offer Paid Credential form", { timeout: 20_000 }, () => {
  it("posts the billing params and the initial deposit", async () => {
    renderForm();
    await fillPaidOffer({ fee: "1.5", period: "60", target: "30", weight: "0.5", initial: "10" });
    submit();

    expect(await postedProposal()).toEqual({
      type: "offer_paid_credential",
      user_service_cid: "00usc",
      holder,
      id: "paid-1",
      description: "Paid test credential",
      claims: [],
      billing_params: {
        fee_per_day_usd: "1.5",
        billing_period_minutes: 60,
        deposit_target_amount_usd: "30",
        holder_activity_weight: "0.5",
      },
      deposit_initial_amount_usd: "10",
    });
  });

  it("leaves the weight and the initial deposit out when they are empty", async () => {
    renderForm();
    await fillPaidOffer({ fee: "2", period: "1440", target: "60" });
    submit();

    const proposal = await postedProposal();
    expect(proposal.billing_params).toEqual({
      fee_per_day_usd: "2",
      billing_period_minutes: 1440,
      deposit_target_amount_usd: "60",
    });
    expect(proposal).not.toHaveProperty("deposit_initial_amount_usd");
  });

  it.each([
    [{ fee: "0", period: "60", target: "30" }, /Fee per Day \(USD\) must be a number above 0/],
    [{ fee: "1", period: "1.5", target: "30" }, /Billing Period \(minutes\) must be a whole number/],
    [{ fee: "1", period: "60", target: "" }, /Deposit Target \(USD\) must be a number above 0/],
    [
      { fee: "1", period: "60", target: "30", weight: "1.5" },
      /Holder Activity Weight must be a number from 0 to 1/,
    ],
    [
      { fee: "1", period: "60", target: "30", initial: "0" },
      /Initial Deposit \(USD\) must be a number above 0/,
    ],
  ])("refuses %o without posting", async (billing, message) => {
    renderForm();
    await fillPaidOffer(billing);
    submit();

    expect(await screen.findByText(message)).toBeTruthy();
    expect(proposeCalls()).toHaveLength(0);
  });
});

describe("Accept Paid Credential form", { timeout: 20_000 }, () => {
  const openAccept = async () => {
    renderForm();
    await chooseType("Accept Paid Credential");
    await waitForUserService();
    // The form prefills an offer only when exactly one passes its filter, so
    // this shows the free offer was filtered out.
    expect(await screen.findByDisplayValue("00paid-offer")).toBeTruthy();
    expect(screen.queryByDisplayValue("00free-offer")).toBeNull();
    // The AmuletRules cid is prefilled from network-info.
    await waitFor(() => expect(field("Amulet Rules CID")).toHaveProperty("value", "00rules"));
  };

  // TextHelp names the picker after its help text.
  const amuletPicker = () =>
    screen.getByRole("combobox", { name: /^Unlocked Amulets of this governance party/ });

  it("posts the picked Amulets and the transfer context", async () => {
    await openAccept();
    fireEvent.mouseDown(amuletPicker());
    const listbox = await screen.findByRole("listbox");
    // Only this party's own Amulets are offered.
    expect(within(listbox).queryByText(/500\.0000000000 CC/)).toBeNull();
    fireEvent.click(within(listbox).getByRole("option", { name: /100\.0000000000 CC/ }));
    type("Open Mining Round CID", "00round");
    submit();

    expect(await postedProposal()).toEqual({
      type: "accept_paid_credential",
      user_service_cid: "00usc",
      credential_offer_cid: "00paid-offer",
      deposit_amulet_cids: [ownAmuletCid],
      app_transfer_context: {
        amulet_rules_cid: "00rules",
        open_mining_round_cid: "00round",
      },
    });
  });

  it("sends the featured app right when one is given", async () => {
    await openAccept();
    fireEvent.mouseDown(amuletPicker());
    fireEvent.click(
      within(await screen.findByRole("listbox")).getByRole("option", { name: /100\.0000000000 CC/ }),
    );
    type("Open Mining Round CID", "00round");
    type("Featured App Right CID", " 00far ");
    submit();

    expect((await postedProposal()).app_transfer_context).toEqual({
      amulet_rules_cid: "00rules",
      open_mining_round_cid: "00round",
      featured_app_right_cid: "00far",
    });
  });

  it("refuses to post without a deposit Amulet", async () => {
    await openAccept();
    type("Open Mining Round CID", "00round");
    submit();

    expect(await screen.findByText("Pick at least one Deposit Amulet")).toBeTruthy();
    expect(proposeCalls()).toHaveLength(0);
  });
});
