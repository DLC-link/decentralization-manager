import { fireEvent, render, screen, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { authenticatedFetch } from "../api";
import { SnackbarProvider } from "../contexts";
import { PackagesPanel } from "./PackagesPanel";
import type {
  ExpectedVersionsResponse,
  PackageInfo,
  PeerPackageComparison,
} from "../types";

vi.mock("../api", () => ({ authenticatedFetch: vi.fn() }));

const ns = "a".repeat(68);
const hostB = `host-b::${ns}`;

const amuletOld: PackageInfo = { package_id: "am-22", name: "splice-amulet", version: "0.1.22" };
const amuletNew: PackageInfo = { package_id: "am-23", name: "splice-amulet", version: "0.1.23" };
const cbtc: PackageInfo = { package_id: "cbtc-1", name: "cbtc", version: "1.0.0" };

const expected: ExpectedVersionsResponse = {
  source: "DSO AmuletRules",
  fetched_at: 1_790_000_000,
  packages: [
    { package_name: "splice-amulet", version: "0.1.23" },
    { package_name: "splice-wallet", version: "0.1.24" },
  ],
};

const json = (body: unknown, status = 200) =>
  Promise.resolve(new Response(JSON.stringify(body), { status }));

const fetchMock = vi.mocked(authenticatedFetch);

const serve = (expectedReply: () => Promise<Response>) => {
  const comparison: PeerPackageComparison = {
    local_packages: [amuletOld, amuletNew, cbtc],
    peers: [{ participant_id: hostB, name: "B", reachable: true, packages: [amuletOld, cbtc] }],
  };
  fetchMock.mockImplementation((input) => {
    const url = String(input);
    if (url.startsWith("/packages/compare-peers")) return json(comparison);
    if (url === "/packages/expected-versions") return expectedReply();
    if (url === "/network-config") return json({ peers: [] });
    return json([]);
  });
};

beforeEach(() => fetchMock.mockReset());

const compare = async () => {
  render(<PackagesPanel />, { wrapper: SnackbarProvider });
  fireEvent.click(screen.getByRole("button", { name: "Check Peer DARs" }));
  await screen.findByTestId("comparison-summary");
};

const expectedCells = () =>
  screen
    .getAllByTestId("expected-version")
    .map((el) => `${el.getAttribute("data-pkg")}=${el.textContent}`);

describe("PackagesPanel expected versions", () => {
  it("shows the DSO's version on Splice rows and none on a domain package", async () => {
    serve(() => json(expected));
    await compare();

    await screen.findAllByText("0.1.23", { selector: "[data-testid=expected-version] *" });
    expect(expectedCells()).toEqual([
      "cbtc=—",
      "splice-amulet=0.1.23",
      "splice-amulet=0.1.23",
    ]);
  });

  it("names an expected version this node does not hold", async () => {
    // The rows list only packages this node holds, so a missing expected
    // version has to be said outright or it never shows.
    serve(() => json(expected));
    await compare();

    const warning = await screen.findByTestId("expected-not-held");
    expect(warning.textContent).toContain("splice-wallet 0.1.24");
    expect(warning.textContent).not.toContain("splice-amulet");
  });

  it("still compares observed versions when the DSO cannot be read", async () => {
    serve(() => json({ error: "DSO API returned 503" }, 502));
    await compare();

    expect(await screen.findByText(/Expected versions are unavailable: DSO API returned 503/)).toBeTruthy();
    const summary = screen.getByTestId("comparison-summary");
    expect(within(summary).getByText(/missing package/)).toBeTruthy();
    expect(expectedCells().every((c) => c.endsWith("=—"))).toBe(true);
  });
});
