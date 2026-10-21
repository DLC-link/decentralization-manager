import { fireEvent, render, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { authenticatedFetch } from "../api";
import { SnackbarProvider } from "../contexts";
import { NetworkConfigAccordion } from "./NetworkConfigAccordion";
import type { DecentralizedParty, NetworkConfig, NodeConfig } from "../types";

vi.mock("../api", () => ({ authenticatedFetch: vi.fn() }));

const ns = "a".repeat(68);
const self = `self::${ns}`;
const bravo = `bravo::${ns}`;
const charlie = `charlie::${ns}`;

const party = (prefix: string, hosts: string[]): DecentralizedParty => ({
  party_id: `${prefix}::${ns}`,
  threshold: 1,
  owners: [],
  participants: hosts.map((participant_uid) => ({
    participant_uid,
    permission: "submission",
  })),
  contracts: [],
});

const config: NetworkConfig = {
  peers: [
    { participant_id: bravo, name: "Bravo", address: "b", port: 1, public_key: "" },
    { participant_id: charlie, name: "Charlie", address: "c", port: 1, public_key: "" },
  ],
};

const nodeConfig = {
  node: { participant_id: self, listen_address: "0.0.0.0", port: 1 },
} as NodeConfig;

const json = (body: unknown) =>
  Promise.resolve(new Response(JSON.stringify(body), { status: 200 }));

const scrollIntoView = vi.fn();

beforeEach(() => {
  vi.mocked(authenticatedFetch).mockImplementation((input) =>
    String(input) === "/decentralized-parties"
      ? json({
          parties: [
            party("alpha", [self, bravo]),
            party("beta", [self, bravo]),
            party("gamma", [self]),
          ],
          total: 3,
        })
      : json({}),
  );
  Element.prototype.scrollIntoView = scrollIntoView;
});

afterEach(() => {
  vi.mocked(authenticatedFetch).mockReset();
  scrollIntoView.mockReset();
});

const renderTable = (
  props: Partial<Parameters<typeof NetworkConfigAccordion>[0]> = {},
) =>
  render(
    <NetworkConfigAccordion config={config} nodeConfig={nodeConfig} {...props} />,
    { wrapper: SnackbarProvider },
  );

const row = (participantId: string) =>
  document.querySelector<HTMLElement>(
    `tr[data-participant-uid="${participantId}"]`,
  )!;

describe("NetworkConfigAccordion peer focus", () => {
  it("marks and scrolls to the row the URL points at", () => {
    renderTable({ focusParticipantId: charlie });

    expect(row(charlie).getAttribute("aria-current")).toBe("true");
    expect(row(bravo).hasAttribute("aria-current")).toBe(false);
    expect(scrollIntoView).toHaveBeenCalledTimes(1);
    expect(scrollIntoView.mock.contexts[0]).toBe(row(charlie));
  });

  it("marks no row without a focus", () => {
    renderTable();

    expect(document.querySelector("tr[aria-current]")).toBeNull();
    expect(scrollIntoView).not.toHaveBeenCalled();
  });
});

describe("NetworkConfigAccordion shared parties", () => {
  it("counts each peer's shared parties and opens them", async () => {
    const onShowSharedParties = vi.fn();
    renderTable({ onShowSharedParties });

    const bravoChip = await within(row(bravo)).findByRole("button", {
      name: "2 shared",
    });
    expect(
      within(row(charlie)).getByRole("button", { name: "0 shared" }),
    ).toBeTruthy();
    expect(within(row(self)).queryByText(/shared/)).toBeNull();

    fireEvent.click(bravoChip);
    expect(onShowSharedParties).toHaveBeenCalledWith(bravo);
  });
});
