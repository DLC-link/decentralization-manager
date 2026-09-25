import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { authenticatedFetch } from "../api";
import { SnackbarProvider } from "../contexts";
import { PackagesPanel } from "./PackagesPanel";
import type { PackageInfo, PeerPackageComparison } from "../types";

vi.mock("../api", () => ({ authenticatedFetch: vi.fn() }));
vi.mock("../constants", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../constants")>()),
  ADMIN_ACCESS: true,
}));

const ns = "a".repeat(68);
const hostB = `host-b::${ns}`;
const hostC = `host-c::${ns}`;

const registry: PackageInfo = { package_id: "reg-4", name: "utility-registry-v0", version: "0.4.0" };

const json = (body: unknown, status = 200) =>
  Promise.resolve(new Response(JSON.stringify(body), { status }));

const fetchMock = vi.mocked(authenticatedFetch);

const serve = (distributeStatus = 202) => {
  const comparison: PeerPackageComparison = {
    local_packages: [registry],
    peers: [
      { participant_id: hostB, name: "Operator B", reachable: true, packages: [] },
      { participant_id: hostC, name: "", reachable: true, packages: [] },
    ],
  };
  fetchMock.mockImplementation((input) => {
    const url = String(input);
    if (url.startsWith("/packages/compare-peers")) return json(comparison);
    if (url === "/network-config") {
      return json({ peers: [{ participant_id: hostB, name: "Operator B" }] });
    }
    if (url === "/dars/distribute-package") {
      return json(distributeStatus === 202 ? { status: "inprogress" } : { error: "busy" }, distributeStatus);
    }
    return json([]);
  });
};

beforeEach(() => fetchMock.mockReset());

const compare = async () => {
  render(<PackagesPanel />, { wrapper: SnackbarProvider });
  fireEvent.click(screen.getByRole("button", { name: "Check Peer DARs" }));
  await screen.findByTestId("comparison-summary");
};

const distributeCalls = () =>
  fetchMock.mock.calls.filter(([input]) => String(input) === "/dars/distribute-package");

describe("PackagesPanel Distribute DAR", () => {
  it("starts the existing distribution for the one participant that misses it", async () => {
    serve();
    await compare();

    fireEvent.click(
      await screen.findByRole("button", { name: "Distribute utility-registry-v0 to Operator B" }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Distribute" }));

    await waitFor(() => expect(distributeCalls()).toHaveLength(1));
    const [, init] = distributeCalls()[0];
    expect(JSON.parse(String(init?.body))).toEqual({
      package_id: "reg-4",
      peer_ids: [hostB],
    });
    expect(await screen.findByText(/Distribution of utility-registry-v0 started/)).toBeTruthy();
  });

  it("sends nothing until the operator confirms", async () => {
    serve();
    await compare();

    fireEvent.click(
      await screen.findByRole("button", { name: "Distribute utility-registry-v0 to Operator B" }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    expect(distributeCalls()).toHaveLength(0);
  });

  it("cannot distribute to a host that is not a configured peer", async () => {
    // Distribution runs over the peer connection, so a host known only from
    // the topology has nowhere to send the invitation.
    serve();
    await compare();

    const button = await screen.findByRole("button", {
      name: "Distribute utility-registry-v0 to host-c",
    });
    expect((button as HTMLButtonElement).disabled).toBe(true);
  });

  it("shows the server's reason when the run does not start", async () => {
    serve(409);
    await compare();

    fireEvent.click(
      await screen.findByRole("button", { name: "Distribute utility-registry-v0 to Operator B" }),
    );
    fireEvent.click(screen.getByRole("button", { name: "Distribute" }));

    expect(await screen.findByText("busy")).toBeTruthy();
  });
});
