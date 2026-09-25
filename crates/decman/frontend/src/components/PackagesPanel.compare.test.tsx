import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { authenticatedFetch } from "../api";
import { SnackbarProvider } from "../contexts";
import { PackagesPanel } from "./PackagesPanel";
import type { PackageInfo, PeerPackageComparison } from "../types";

vi.mock("../api", () => ({ authenticatedFetch: vi.fn() }));

const ns = "a".repeat(68);
const hostB = `host-b::${ns}`;

const registry: PackageInfo = { package_id: "reg-4", name: "utility-registry-v0", version: "0.4.0" };
const cbtc: PackageInfo = { package_id: "cbtc-1", name: "cbtc", version: "1.0.0" };

const json = (body: unknown) =>
  Promise.resolve(new Response(JSON.stringify(body), { status: 200 }));

const fetchMock = vi.mocked(authenticatedFetch);

const serve = (peerPackages: PackageInfo[]) => {
  const comparison: PeerPackageComparison = {
    local_packages: [registry, cbtc],
    peers: [{ participant_id: hostB, name: "Operator B", reachable: true, packages: peerPackages }],
  };
  fetchMock.mockImplementation((input) => {
    const url = String(input);
    if (url.startsWith("/packages/compare-peers")) return json(comparison);
    if (url === "/network-config") return json({ peers: [] });
    return json([]);
  });
};

beforeEach(() => fetchMock.mockReset());

const compare = async () => {
  fireEvent.click(screen.getByRole("button", { name: "Check Peer DARs" }));
  return screen.findByTestId("comparison-summary");
};

const statuses = () =>
  screen
    .queryAllByTestId("peer-dar-status")
    .map((el) => `${el.getAttribute("data-pkg")}:${el.getAttribute("data-status")}`);

describe("PackagesPanel comparison", () => {
  it("says no differences were found for this selection, not that all is in sync", async () => {
    serve([registry, cbtc]);
    render(<PackagesPanel />, { wrapper: SnackbarProvider });

    const summary = await compare();

    expect(summary.textContent).toContain(
      "No version differences found for the selected participants and package set",
    );
  });

  it("marks an older version apart from a missing package", async () => {
    serve([{ ...registry, package_id: "reg-3", version: "0.3.0" }]);
    render(<PackagesPanel />, { wrapper: SnackbarProvider });

    await compare();

    await waitFor(() =>
      expect(statuses()).toEqual(["cbtc:missing", "utility-registry-v0:other_version"]),
    );
    expect(screen.getByText("0.3.0")).toBeTruthy();
  });

  it("narrows the comparison to a quick-filter group", async () => {
    serve([registry]);
    render(<PackagesPanel />, { wrapper: SnackbarProvider });
    await compare();

    fireEvent.click(screen.getByTestId("package-group-registry"));

    await waitFor(() => expect(statuses()).toEqual(["utility-registry-v0:match"]));
    expect(screen.getByTestId("comparison-summary").textContent).toContain(
      "No version differences found",
    );
  });

  it("hides matching rows when differences only is on", async () => {
    serve([registry]);
    render(<PackagesPanel />, { wrapper: SnackbarProvider });
    await compare();

    fireEvent.click(screen.getByRole("switch", { name: "Differences only" }));

    await waitFor(() => expect(statuses()).toEqual(["cbtc:missing"]));
  });
});
