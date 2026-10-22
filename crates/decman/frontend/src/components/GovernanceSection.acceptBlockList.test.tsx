import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { SnackbarProvider } from "../contexts";
import { GovernanceSection } from "./GovernanceSection";

// The Accept Mint Request and Accept Burn Request forms send block list
// shards when the instrument checks a block list (#566). These tests drive
// each form to a submit and read the request body it posts.
const ns = "a".repeat(68);
const self = `gov::${ns}`;
const operator = `operator::${ns}`;
const holder = `holder::${ns}`;
const otherRegistrar = `other::${ns}`;

const instrument = (id: string, check_block_list: boolean) => ({
  contract_id: `00icc-${id}`,
  instrument_admin: self,
  instrument_id: id,
  configuration: {
    additional_identifiers: [],
    issuer_requirements: [],
    holder_requirements: [],
    check_block_list,
  },
});

const request = (prefix: string, instrumentId: string) => ({
  contract_id: `${prefix}-${instrumentId}`,
  holder,
  amount: "5.0000000000",
  instrument_admin: self,
  instrument_id: instrumentId,
  expires_at: 0,
});

const shard = (contract_id: string, registrar: string, prefix: string) => ({
  contract_id,
  operator,
  registrar,
  prefix,
});

const ownShards = [shard("00shard-zero", self, "0"), shard("00shard-one", self, "1")];
const foreignShard = shard("00shard-foreign", otherRegistrar, "2");

const { authenticatedFetch } = vi.hoisted(() => ({ authenticatedFetch: vi.fn() }));
vi.mock("../api", () => ({ authenticatedFetch }));

const ok = (body: unknown) => Promise.resolve({ ok: true, json: async () => body });

const mockLedger = (shards: object[]) => {
  authenticatedFetch.mockReset();
  authenticatedFetch.mockImplementation((url: string, init?: RequestInit) => {
    if (url.includes("/governance/confirmations")) {
      return ok({ rules_contract_id: "rules-1", threshold: 2, domain_actions: [] });
    }
    if (url.includes("/instruments")) {
      return ok({ instruments: [instrument("CHECKED", true), instrument("PLAIN", false)] });
    }
    if (url.includes("/governance/mint-requests")) {
      return ok({ mint_requests: [request("00mr", "CHECKED"), request("00mr", "PLAIN")] });
    }
    if (url.includes("/governance/burn-requests")) {
      return ok({ burn_requests: [request("00br", "CHECKED"), request("00br", "PLAIN")] });
    }
    if (url.includes("/block-list-shards")) {
      return ok({ block_list_shards: shards });
    }
    if (url.includes("/credentials")) {
      return ok({ credentials: [] });
    }
    if (url.includes("/governance/propose") && init?.method === "POST") {
      return ok({});
    }
    // Every other fetch stays pending, so only the form is under test.
    return new Promise(() => {});
  });
};

beforeEach(() => mockLedger([...ownShards, foreignShard]));

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

// TextHelp names each picker after its help text.
const shardPicker = () =>
  screen.queryByRole("combobox", { name: /^The registry checks the holder against these shards/ });

const submit = () =>
  fireEvent.click(screen.getAllByRole("button", { name: /Submit Proposal/ })[0]);

const proposeCalls = () =>
  authenticatedFetch.mock.calls.filter(([url]) => String(url).includes("/governance/propose"));

const postedProposal = async () => {
  await waitFor(() => expect(proposeCalls()).toHaveLength(1));
  return JSON.parse(String(proposeCalls()[0][1]?.body)).proposal;
};

const forms = [
  {
    option: "Accept Mint Request",
    type: "accept_mint_request",
    picker: /^MintRequest contract created by the holder/,
  },
  {
    option: "Accept Burn Request",
    type: "accept_burn_request",
    picker: /^BurnRequest contract created by the holder/,
  },
];

// Picking a request prefills its instrument. Wait for the shards first, so
// the prefill can read them.
const openWithRequest = async (form: (typeof forms)[number], instrumentId: string) => {
  renderForm();
  await chooseType(form.option);
  await waitFor(() =>
    expect(authenticatedFetch).toHaveBeenCalledWith(expect.stringContaining("/block-list-shards")),
  );
  fireEvent.mouseDown(screen.getByRole("combobox", { name: form.picker }));
  fireEvent.click(
    await within(await screen.findByRole("listbox")).findByRole("option", {
      name: new RegExp(`${instrumentId} `),
    }),
  );
};

// GovernanceSection is large, so give each render room on a loaded CI runner.
describe("accept request block list shards", { timeout: 20_000 }, () => {
  for (const form of forms) {
    it(`${form.type}: sends the registrar's shards when the instrument checks a block list`, async () => {
      await openWithRequest(form, "CHECKED");
      const picker = shardPicker();
      expect(picker).not.toBeNull();
      // Only the instrument registrar's shards are offered.
      fireEvent.mouseDown(picker as HTMLElement);
      const listbox = await screen.findByRole("listbox");
      expect(within(listbox).getAllByRole("option").map((o) => o.textContent)).toEqual([
        "Prefix 0 (…ard-zero)",
        "Prefix 1 (…hard-one)",
      ]);
      submit();

      const proposal = await postedProposal();
      expect(proposal.type).toBe(form.type);
      expect(proposal.instrument_configuration_cid).toBe("00icc-CHECKED");
      expect(proposal.block_list_shard_cids).toEqual(["00shard-zero", "00shard-one"]);
    });

    it(`${form.type}: has no shard picker and sends no shards without the check`, async () => {
      await openWithRequest(form, "PLAIN");
      expect(shardPicker()).toBeNull();
      submit();

      const proposal = await postedProposal();
      expect(proposal.instrument_configuration_cid).toBe("00icc-PLAIN");
      expect(proposal.block_list_shard_cids).toEqual([]);
    });

    // The request can be picked before /block-list-shards answers. The
    // registrar's shards must still be filled in once they arrive.
    it(`${form.type}: fills in shards that arrive after the request is picked`, async () => {
      let releaseShards: () => void = () => {};
      const shardsReady = new Promise<void>((resolve) => {
        releaseShards = resolve;
      });
      const answer = authenticatedFetch.getMockImplementation();
      authenticatedFetch.mockImplementation((url: string, init?: RequestInit) =>
        url.includes("/block-list-shards")
          ? shardsReady.then(() => ({
              ok: true,
              json: async () => ({ block_list_shards: [...ownShards, foreignShard] }),
            }))
          : answer?.(url, init),
      );

      await openWithRequest(form, "CHECKED");
      releaseShards();
      // Each selected shard shows as a chip labelled with its prefix.
      expect(await screen.findByText("Prefix 0 (…ard-zero)")).toBeTruthy();
      expect(screen.getByText("Prefix 1 (…hard-one)")).toBeTruthy();
      submit();

      const proposal = await postedProposal();
      expect(proposal.block_list_shard_cids).toEqual(["00shard-zero", "00shard-one"]);
    });

    it(`${form.type}: refuses to post without a shard when the instrument checks a block list`, async () => {
      mockLedger([foreignShard]);
      await openWithRequest(form, "CHECKED");
      expect(shardPicker()).not.toBeNull();
      submit();

      expect(await screen.findByText("Pick at least one Block List Shard")).toBeTruthy();
      expect(proposeCalls()).toHaveLength(0);
    });
  }
});
