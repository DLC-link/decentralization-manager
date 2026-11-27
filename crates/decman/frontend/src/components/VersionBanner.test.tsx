import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { useUpgradeNotice } from "../useUpgradeNotice";
import { VersionBanner } from "./VersionBanner";

const Harness = ({ current }: { current: string }) => {
  const { notice, dismiss } = useUpgradeNotice(current);
  return notice ? <VersionBanner notice={notice} onDismiss={dismiss} /> : null;
};

// GitHub is the external boundary: stub the browser's fetch of the policy.
const servePolicy = (latest: string, minimum_supported: string) => {
  const fetchMock = vi.fn(() =>
    Promise.resolve({ ok: true, json: () => Promise.resolve({ latest, minimum_supported }) }),
  );
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
};

// Lets the fetch → json → setState chain settle before an absence check.
const settle = () => act(() => new Promise((resolve) => setTimeout(resolve, 0)));

beforeEach(() => localStorage.clear());
afterEach(() => vi.unstubAllGlobals());

describe("VersionBanner", () => {
  it("shows a dismissable update that stays hidden after a reload", async () => {
    servePolicy("1.18.0", "1.14.0");
    const first = render(<Harness current="1.17.1" />);

    const banner = await screen.findByTestId("version-banner-update");
    expect(banner.textContent).toContain("Decentralization Manager v1.18.0 is out");
    expect(banner.textContent).toContain("This node runs v1.17.1");
    expect(within(banner).getByRole("link", { name: "Release notes" }).getAttribute("href")).toBe(
      "https://github.com/DLC-link/decentralization-manager/releases/tag/v1.18.0",
    );

    fireEvent.click(within(banner).getByRole("button", { name: /close/i }));
    expect(screen.queryByTestId("version-banner-update")).toBeNull();

    first.unmount();
    render(<Harness current="1.17.1" />);
    await settle();
    expect(screen.queryByTestId("version-banner-update")).toBeNull();
  });

  it("shows the update again when a newer release follows a dismissed one", async () => {
    servePolicy("1.18.0", "1.14.0");
    const first = render(<Harness current="1.17.1" />);
    fireEvent.click(
      within(await screen.findByTestId("version-banner-update")).getByRole("button", {
        name: /close/i,
      }),
    );
    first.unmount();

    servePolicy("1.19.0", "1.14.0");
    render(<Harness current="1.17.1" />);
    expect((await screen.findByTestId("version-banner-update")).textContent).toContain("v1.19.0");
  });

  it("shows a critical banner that has no close button and ignores a dismissal", async () => {
    localStorage.setItem("dismissed-update-version", "1.18.0");
    servePolicy("1.18.0", "1.16.0");
    render(<Harness current="1.15.0" />);

    const banner = await screen.findByTestId("version-banner-critical");
    expect(banner.textContent).toContain("Upgrade required.");
    expect(banner.textContent).toContain("The minimum supported version is v1.16.0");
    expect(banner.textContent).toContain("Upgrade to v1.18.0");
    expect(within(banner).queryByRole("button")).toBeNull();
  });

  it("shows nothing when the policy cannot be read", async () => {
    vi.stubGlobal("fetch", () => Promise.reject(new Error("offline")));
    render(<Harness current="1.0.0" />);
    await settle();
    expect(screen.queryByRole("alert")).toBeNull();
  });
});
