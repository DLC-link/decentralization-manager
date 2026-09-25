import { render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { authenticatedFetch } from "../api";
import { SnackbarProvider } from "../contexts";
import { PackagesPanel } from "./PackagesPanel";
import type { DecentralizedParty, PeerPackageComparison } from "../types";

vi.mock("../api", () => ({ authenticatedFetch: vi.fn() }));

const ns = "a".repeat(68);
const self = `self::${ns}`;
const hostB = `host-b::${ns}`;
const hostC = `host-c::${ns}`;

const party: DecentralizedParty = {
  party_id: `alpha::${ns}`,
  threshold: 2,
  owners: [],
  participants: [self, hostB, hostC].map((participant_uid) => ({
    participant_uid,
    permission: "confirmation",
  })),
  contracts: [],
};

const comparison: PeerPackageComparison = {
  local_packages: [{ package_id: "pkg-a", name: "utility-registry-v0", version: "0.4.0" }],
  peers: [hostB, hostC].map((participant_id) => ({
    participant_id,
    name: "",
    reachable: true,
    packages: [],
  })),
};

const json = (body: unknown) =>
  Promise.resolve(new Response(JSON.stringify(body), { status: 200 }));

const fetchMock = vi.mocked(authenticatedFetch);

beforeEach(() => {
  fetchMock.mockReset();
  fetchMock.mockImplementation((input) => {
    const url = String(input);
    if (url.startsWith("/packages/compare-peers")) return json(comparison);
    if (url === "/network-config") {
      return json({ peers: [{ participant_id: hostB, name: "Operator B" }] });
    }
    return json([]);
  });
});

const compareCalls = () =>
  fetchMock.mock.calls
    .map(([input]) => String(input))
    .filter((url) => url.startsWith("/packages/compare-peers"));

describe("PackagesPanel opened for a party", () => {
  it("compares only the party's hosts other than this node", async () => {
    render(<PackagesPanel party={party} selfParticipantId={self} />, { wrapper: SnackbarProvider });

    await waitFor(() => expect(compareCalls()).toHaveLength(1));
    const params = new URL(compareCalls()[0], "http://x").searchParams;
    expect(params.get("participants")).toBe(`${hostB},${hostC}`);
  });

  it("shows the selected hosts, named where a peer row exists", async () => {
    render(<PackagesPanel party={party} selfParticipantId={self} />, { wrapper: SnackbarProvider });

    expect(await screen.findByText("Operator B")).toBeTruthy();
    expect(screen.getByText("host-c")).toBeTruthy();
    expect(screen.queryByText("self")).toBeNull();
    expect(screen.getByTestId("party-scope-chip").textContent).toContain("alpha");
  });

  it("does not compare on its own without a party", async () => {
    render(<PackagesPanel selfParticipantId={self} />, { wrapper: SnackbarProvider });

    await waitFor(() => expect(fetchMock).toHaveBeenCalled());
    expect(compareCalls()).toHaveLength(0);
  });
});

describe("PackagesPanel party refresh", () => {
  it("keeps the comparison when the parties poll hands over a new object", async () => {
    const { rerender } = render(<PackagesPanel party={party} selfParticipantId={self} />, { wrapper: SnackbarProvider });
    await waitFor(() => expect(compareCalls()).toHaveLength(1));

    rerender(<PackagesPanel party={{ ...party }} selfParticipantId={self} />);

    await waitFor(() => expect(fetchMock).toHaveBeenCalled());
    expect(compareCalls()).toHaveLength(1);
  });
});
