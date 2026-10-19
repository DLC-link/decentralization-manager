import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { DecentralizedParty } from "./types";

// `vi.resetModules` re-runs the factory for each fresh App import, so the
// mock lives outside it and every import shares the one instance.
const { fetchMock } = vi.hoisted(() => ({ fetchMock: vi.fn() }));
vi.mock("./api", () => ({
  authenticatedFetch: fetchMock,
  pingLatency: () => Promise.resolve(null),
}));

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

// Bravo hosts three of them and Charlie two, so each count is told apart.
const parties = [
  party("alpha", [self, bravo, charlie]),
  party("beta", [self, bravo]),
  party("gamma", [self, charlie]),
  party("delta", [self, bravo]),
];
const partyId = (prefix: string) => `${prefix}::${ns}`;

const json = (body: unknown, status = 200) =>
  Promise.resolve(new Response(JSON.stringify(body), { status }));

function backend(input: RequestInfo | URL) {
  const url = new URL(String(input), "http://x");
  switch (url.pathname) {
    case "/node-config":
      return json({
        node: { participant_id: self, listen_address: "0.0.0.0", port: 1 },
        canton: { network: "devnet" },
        test_mode: false,
        version: "0.0.0",
      });
    case "/decentralized-parties": {
      const prefix = url.searchParams.get("prefix");
      const listed = prefix
        ? parties.filter((p) => p.party_id.startsWith(prefix))
        : parties;
      return json({ parties: listed, total: listed.length });
    }
    case "/network-config":
      return json({
        peers: [
          { participant_id: bravo, name: "Bravo", address: "b", port: 1, public_key: "" },
        ],
      });
    case "/keys/status":
      return json({ has_keys: true, public_key: "" });
    case "/auth/status":
      return json({ parties: [] });
    case "/participants-status":
      return json({ statuses: [] });
    case "/invitations":
      return json({ invitations: [] });
    case "/workflows":
      return json({ runs: [] });
    case "/governance/audit":
      return json({ entries: [] });
    case "/packages/vetted":
      return json([]);
    default:
      return json({}, 404);
  }
}

/** Render the whole app as if the browser opened `hash`. */
async function renderApp(hash: string) {
  window.history.replaceState(null, "", hash);
  (window as { __INITIAL_HASH__?: string }).__INITIAL_HASH__ = hash;
  // App reads the starting route once, when its module loads.
  vi.resetModules();
  const { default: App } = await import("./App");
  const { SnackbarProvider, ThemeProvider } = await import("./contexts");
  const { AuthContext } = await import("./contexts/AuthContextValue");
  render(
    <ThemeProvider>
      <SnackbarProvider>
        <AuthContext.Provider value={{ token: null, logout: () => {} }}>
          <App />
        </AuthContext.Provider>
      </SnackbarProvider>
    </ThemeProvider>,
  );
}

const participantRow = async (participantId: string) =>
  (await screen.findAllByTestId("participant-row")).find(
    (row) => row.dataset.participantUid === participantId,
  )!;

const listedParty = (prefix: string) =>
  screen.queryByRole("button", { name: `Open party ${partyId(prefix)}` });

const partyListCalls = () =>
  fetchMock.mock.calls
    .map(([input]) => String(input))
    .filter((url) => url.startsWith("/decentralized-parties"));

beforeEach(() => {
  fetchMock.mockReset();
  fetchMock.mockImplementation(backend);
  window.matchMedia = ((query: string) => ({
    matches: false,
    media: query,
    addEventListener: () => {},
    removeEventListener: () => {},
    addListener: () => {},
    removeListener: () => {},
  })) as unknown as typeof window.matchMedia;
  Element.prototype.scrollIntoView = () => {};
});

describe("party details to shared parties", () => {
  it("lists only the parties this node shares with the participant", async () => {
    await renderApp(`#parties/${partyId("alpha")}`);

    const chip = await within(await participantRow(bravo)).findByRole("button", {
      name: "3 shared",
    });
    fireEvent.click(chip);

    expect(window.location.hash).toBe(`#parties/shared/${bravo}`);
    expect(await screen.findByText("Shared with Bravo")).toBeTruthy();
    await waitFor(() => expect(listedParty("beta")).toBeTruthy());
    expect(listedParty("alpha")).toBeTruthy();
    expect(listedParty("delta")).toBeTruthy();
    expect(listedParty("gamma")).toBeNull();
  });

  it("reads the whole list again when a prefix narrowed it", async () => {
    await renderApp("#parties/alpha");
    fireEvent.click(await screen.findByRole("button", { name: `Open party ${partyId("alpha")}` }));

    // The count covers every party, not only the prefix's one.
    fireEvent.click(
      await within(await participantRow(bravo)).findByRole("button", { name: "3 shared" }),
    );

    await waitFor(() => expect(listedParty("beta")).toBeTruthy());
    expect(listedParty("delta")).toBeTruthy();
    expect(partyListCalls()).toContain("/decentralized-parties?prefix=alpha");
    expect(partyListCalls().at(-1)).toBe("/decentralized-parties");
  });

  it("returns to the shared list from a party opened in it", async () => {
    await renderApp(`#parties/shared/${bravo}`);
    fireEvent.click(await screen.findByRole("button", { name: `Open party ${partyId("beta")}` }));
    await screen.findAllByTestId("participant-row");

    fireEvent.click(screen.getByRole("button", { name: "Back to parties" }));

    expect(window.location.hash).toBe(`#parties/shared/${bravo}`);
    expect(await screen.findByText("Shared with Bravo")).toBeTruthy();
    expect(listedParty("gamma")).toBeNull();
  });

  it("returns to the party details on browser Back", async () => {
    await renderApp(`#parties/${partyId("alpha")}`);
    fireEvent.click(
      await within(await participantRow(charlie)).findByRole("button", { name: "2 shared" }),
    );
    await screen.findByText("Shared with charlie");

    await act(async () => {
      window.history.back();
      await new Promise((resolve) => window.addEventListener("popstate", resolve, { once: true }));
    });

    expect(await screen.findAllByTestId("participant-row")).toHaveLength(3);
    expect(screen.queryByText("Shared with charlie")).toBeNull();
  });
});

describe("shared parties deep link", () => {
  it("opens the filtered list, and the chip clears the filter", async () => {
    await renderApp(`#parties/shared/${charlie}`);

    // Charlie is not a configured peer, so the chip names its id's prefix.
    const chip = await screen.findByTestId("shared-scope-chip");
    expect(chip.textContent).toBe("Shared with charlie");
    await waitFor(() => expect(listedParty("gamma")).toBeTruthy());
    expect(listedParty("alpha")).toBeTruthy();
    expect(listedParty("beta")).toBeNull();

    fireEvent.click(within(chip).getByTestId("CancelIcon"));

    expect(window.location.hash).toBe("#parties");
    expect(screen.queryByText(/Shared with/)).toBeNull();
    expect(listedParty("beta")).toBeTruthy();
  });
});

describe("peer config to shared parties", () => {
  it("shows the decentralized list even when the external one was open", async () => {
    await renderApp("#parties");
    fireEvent.click(await screen.findByRole("tab", { name: "External Parties" }));
    await act(async () => {
      window.history.pushState(null, "", "#config");
      window.dispatchEvent(new PopStateEvent("popstate"));
    });
    const bravoRow = await waitFor(() => {
      const row = document.querySelector<HTMLElement>(
        `tr[data-participant-uid="${bravo}"]`,
      );
      expect(row).toBeTruthy();
      return row!;
    });

    fireEvent.click(await within(bravoRow).findByRole("button", { name: "3 shared" }));

    expect(await screen.findByTestId("shared-scope-chip")).toBeTruthy();
    await waitFor(() => expect(listedParty("beta")).toBeTruthy());
  });
});

describe("party details to peer config", () => {
  it("opens the configuration tab at the peer's row", async () => {
    await renderApp(`#parties/${partyId("alpha")}`);

    fireEvent.click(
      await within(await participantRow(bravo)).findByRole("button", { name: "Bravo" }),
    );

    expect(window.location.hash).toBe(`#config/${bravo}`);
    await waitFor(() =>
      expect(
        document
          .querySelector(`tr[data-participant-uid="${bravo}"]`)
          ?.getAttribute("aria-current"),
      ).toBe("true"),
    );
  });
});
