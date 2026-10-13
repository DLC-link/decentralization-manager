import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { authenticatedFetch } from "../api";
import { SnackbarProvider } from "../contexts";
import { FormerPartyDetail } from "./FormerPartyDetail";
import type { FormerParty, PurgePreview, PurgeStatus } from "../types";

const access = vi.hoisted(() => ({ admin: true }));

vi.mock("../api", () => ({ authenticatedFetch: vi.fn() }));
vi.mock("../constants", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../constants")>()),
  get ADMIN_ACCESS() {
    return access.admin;
  },
}));

const fetchMock = vi.mocked(authenticatedFetch);

const PARTY_ID = `old-pool::1220${"c".repeat(64)}`;
const ENCODED = encodeURIComponent(PARTY_ID);
const PREVIEW_URL = `/former-parties/purge-preview?party_id=${ENCODED}`;
const PURGE_URL = "/former-parties/purge";
const RECONNECT_URL = "/former-parties/reconnect";
const HEALTH_URL = "/node-health";
const DISMISS_URL = `/former-parties?party_id=${ENCODED}`;

const formerParty = (
  purge_status: PurgeStatus,
  over: Partial<FormerParty> = {},
): FormerParty => ({
  party_id: PARTY_ID,
  removed_at: 1_791_000_000,
  detected_at: 1_791_000_060,
  purge_status,
  ...over,
});

const json = (body: unknown, status = 200) =>
  Promise.resolve(new Response(JSON.stringify(body), { status }));

const serve = ({
  preview = { party_id: PARTY_ID, purge_count: 37, keep_count: 2 },
  previewStatus = 200,
  purgeStatus = 202,
  purgeBody = { message: "Purge started" },
  connected = true,
  reconnectStatus = 200,
  reconnectBody = { message: "The participant is connected and healthy again" },
}: {
  preview?: PurgePreview | { error: string };
  previewStatus?: number;
  purgeStatus?: number;
  purgeBody?: unknown;
  connected?: boolean;
  reconnectStatus?: number;
  reconnectBody?: unknown;
} = {}) =>
  fetchMock.mockImplementation((input, init) => {
    const url = String(input);
    if (url === PREVIEW_URL) return json(preview, previewStatus);
    if (url === PURGE_URL && init?.method === "POST") {
      return json(purgeBody, purgeStatus);
    }
    if (url === HEALTH_URL) {
      return json({
        synchronizers: connected
          ? [{ physical_synchronizer_id: "global::1220", healthy: true }]
          : [],
      });
    }
    if (url === RECONNECT_URL && init?.method === "POST") {
      return json(reconnectBody, reconnectStatus);
    }
    if (url === DISMISS_URL && init?.method === "DELETE") {
      return json({ message: "dismissed" });
    }
    return json({ error: `unexpected ${init?.method ?? "GET"} ${url}` }, 500);
  });

const callsTo = (url: string, method?: string) =>
  fetchMock.mock.calls.filter(
    ([input, init]) =>
      String(input) === url && (method === undefined || init?.method === method),
  );

const renderDetail = (party: FormerParty) => {
  const onBack = vi.fn();
  const onChanged = vi.fn();
  const onPurgeStarted = vi.fn();
  render(
    <FormerPartyDetail
      party={party}
      onBack={onBack}
      onChanged={onChanged}
      onPurgeStarted={onPurgeStarted}
    />,
    { wrapper: SnackbarProvider },
  );
  return { onBack, onChanged, onPurgeStarted };
};

const openAdvanced = () =>
  fireEvent.click(screen.getByRole("button", { name: "Advanced" }));

const openPurge = async (label = "Purge") => {
  openAdvanced();
  fireEvent.click(await screen.findByRole("button", { name: label }));
  await screen.findByRole("dialog");
};

/** Tick the box and type the party's name. */
const confirmTwice = (name = "old-pool") => {
  fireEvent.click(screen.getByRole("checkbox"));
  fireEvent.change(screen.getByLabelText("Type old-pool to confirm"), {
    target: { value: name },
  });
};

const purgeButton = () =>
  screen.getByRole("button", {
    name: "Purge 37 contracts",
  }) as HTMLButtonElement;

beforeEach(() => {
  fetchMock.mockReset();
  access.admin = true;
});

describe("FormerPartyDetail", () => {
  it("counts the contracts when the purge dialog opens, and only then", async () => {
    let answer!: (response: Response) => void;
    fetchMock.mockImplementation(
      () => new Promise<Response>((resolve) => (answer = resolve)),
    );
    renderDetail(formerParty("none"));
    expect(screen.getByTestId("former-party-title").textContent).toBe("old-pool");
    expect(fetchMock).not.toHaveBeenCalled();

    await openPurge();
    expect(callsTo(PREVIEW_URL)).toHaveLength(1);
    expect(screen.getByText("Counting contracts…")).toBeTruthy();
    const pending = screen.getByRole("button", { name: "Purge" }) as HTMLButtonElement;
    expect(pending.closest("[role=dialog]")).toBeTruthy();
    expect(pending.disabled).toBe(true);

    answer(
      new Response(
        JSON.stringify({ party_id: PARTY_ID, purge_count: 37, keep_count: 2 }),
      ),
    );
    expect(
      await screen.findByText(
        "This permanently deletes 37 contracts of old-pool from this node",
      ),
    ).toBeTruthy();
    expect(
      screen.getByText(
        "It keeps 2 contracts that another party on this node is a stakeholder of. A deleted contract cannot be brought back.",
      ),
    ).toBeTruthy();
    const dialog = screen.getByRole("dialog");
    expect(dialog.textContent).toMatch(/DecMan reconnects the participant when the purge ends/);
    expect(dialog.textContent).toMatch(/Do not purge right after the participant recovers/);
  });

  it("keeps the purge in the collapsed Advanced section", async () => {
    renderDetail(formerParty("none"));
    expect(screen.queryByRole("button", { name: "Purge" })).toBeNull();
    expect(screen.queryByText("Danger zone")).toBeNull();

    openAdvanced();

    expect(await screen.findByText("Danger zone")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Purge" })).toBeTruthy();
  });

  it("starts the purge only after the box is ticked and the name typed", async () => {
    serve();
    renderDetail(formerParty("none"));
    await openPurge();
    await screen.findByRole("button", { name: "Purge 37 contracts" });
    expect(purgeButton().disabled).toBe(true);

    fireEvent.click(screen.getByRole("checkbox"));
    expect(purgeButton().disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("Type old-pool to confirm"), {
      target: { value: "old-poo" },
    });
    expect(purgeButton().disabled).toBe(true);
    fireEvent.change(screen.getByLabelText("Type old-pool to confirm"), {
      target: { value: "old-pool" },
    });
    expect(purgeButton().disabled).toBe(false);

    fireEvent.click(screen.getByRole("checkbox"));
    expect(purgeButton().disabled).toBe(true);
  });

  it("asks for both confirmations again on each opening", async () => {
    serve();
    renderDetail(formerParty("none"));
    await openPurge();
    await screen.findByRole("button", { name: "Purge 37 contracts" });
    confirmTwice();
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());

    fireEvent.click(screen.getByRole("button", { name: "Purge" }));
    await screen.findByRole("button", { name: "Purge 37 contracts" });
    expect((screen.getByRole("checkbox") as HTMLInputElement).checked).toBe(false);
    expect(
      (screen.getByLabelText("Type old-pool to confirm") as HTMLInputElement).value,
    ).toBe("");
    expect(purgeButton().disabled).toBe(true);
  });

  it("sends no purge when the operator cancels", async () => {
    serve();
    renderDetail(formerParty("none"));
    await openPurge();
    await screen.findByRole("button", { name: "Purge 37 contracts" });

    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));

    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(callsTo(PURGE_URL)).toHaveLength(0);
  });

  it("starts the purge on confirm", async () => {
    serve();
    const { onChanged, onPurgeStarted } = renderDetail(formerParty("none"));
    await openPurge();
    await screen.findByRole("button", { name: "Purge 37 contracts" });
    confirmTwice();
    fireEvent.click(purgeButton());

    expect(await screen.findByText("Purge started")).toBeTruthy();
    const posts = callsTo(PURGE_URL, "POST");
    expect(posts).toHaveLength(1);
    expect(JSON.parse(String(posts[0][1]?.body))).toEqual({ party_id: PARTY_ID });
    expect(onPurgeStarted).toHaveBeenCalledTimes(1);
    expect(onChanged).not.toHaveBeenCalled();
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("shows the server's reason when the purge is refused, and stays open", async () => {
    const reason =
      "workflows are running on this node (Kick k-1). A purge takes the participant off the synchronizer, so wait for them to finish";
    serve({ purgeStatus: 409, purgeBody: { error: reason } });
    const { onPurgeStarted } = renderDetail(formerParty("none"));
    await openPurge();
    await screen.findByRole("button", { name: "Purge 37 contracts" });
    confirmTwice();
    fireEvent.click(purgeButton());

    expect(await screen.findByText(reason)).toBeTruthy();
    expect(screen.getByRole("dialog")).toBeTruthy();
    expect(onPurgeStarted).not.toHaveBeenCalled();
    const confirm = screen.getByRole("button", {
      name: "Purge 37 contracts",
    }) as HTMLButtonElement;
    await waitFor(() => expect(confirm.disabled).toBe(false));
  });

  it("offers to mark the party purged when nothing is left to remove", async () => {
    serve({ preview: { party_id: PARTY_ID, purge_count: 0, keep_count: 5 } });
    const { onPurgeStarted } = renderDetail(formerParty("none"));
    await openPurge();

    expect(
      await screen.findByText(
        "Nothing to purge. Every contract of this party is shared with a party this node still hosts.",
      ),
    ).toBeTruthy();
    expect(screen.getByRole("dialog").textContent).not.toMatch(/leaves the synchronizer/);
    expect(screen.queryByRole("checkbox")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Mark as purged" }));

    await screen.findByText("Purge started");
    expect(callsTo(PURGE_URL, "POST")).toHaveLength(1);
    expect(onPurgeStarted).toHaveBeenCalledTimes(1);
  });

  it("shows why the count failed and keeps confirm disabled", async () => {
    serve({
      preview: { error: "This node has no ledger credentials for old-pool" },
      previewStatus: 409,
    });
    renderDetail(formerParty("none"));
    await openPurge();

    expect(
      await screen.findByText("This node has no ledger credentials for old-pool"),
    ).toBeTruthy();
    const confirm = screen.getByRole("button", { name: "Purge" }) as HTMLButtonElement;
    expect(confirm.closest("[role=dialog]")).toBeTruthy();
    expect(confirm.disabled).toBe(true);
  });

  it.each(["none", "running", "failed"] as const)(
    "offers no Dismiss while the purge status is %s",
    (status) => {
      renderDetail(formerParty(status, { purge_error: "boom" }));
      expect(screen.queryByRole("button", { name: "Dismiss" })).toBeNull();
    },
  );

  it("dismisses a purged party and returns to the list", async () => {
    serve();
    const { onBack, onChanged } = renderDetail(
      formerParty("succeeded", { purged_count: 128, kept_count: 4 }),
    );
    expect(
      screen.getByText(
        "Purged 128 contracts. Kept 4 that another party on this node is a stakeholder of.",
      ),
    ).toBeTruthy();
    expect(screen.queryByRole("button", { name: /purge/i })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));

    expect(await screen.findByText("Dismissed")).toBeTruthy();
    const deletes = callsTo(DISMISS_URL, "DELETE");
    expect(deletes).toHaveLength(1);
    expect(String(deletes[0][0])).toContain("old-pool%3A%3A1220");
    expect(onChanged).toHaveBeenCalledTimes(1);
    expect(onBack).toHaveBeenCalledTimes(1);
  });

  it("shows a running purge without a way to start another", () => {
    renderDetail(formerParty("running"));
    expect(screen.getByText(/Purge in progress/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Purge" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Retry purge" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Dismiss" })).toBeNull();
  });

  it("offers a retry after a failed purge, with the error", async () => {
    serve();
    renderDetail(formerParty("failed", { purge_error: "reconnect timed out" }));
    expect(screen.getByText("The last purge failed: reconnect timed out")).toBeTruthy();
    openAdvanced();
    expect(await screen.findByRole("button", { name: "Retry purge" })).toBeTruthy();
  });

  it("disables the actions without admin access", async () => {
    access.admin = false;
    renderDetail(formerParty("none"));
    openAdvanced();
    const purge = (await screen.findByRole("button", {
      name: "Purge",
    })) as HTMLButtonElement;
    expect(purge.disabled).toBe(true);
  });

  it("offers a reconnect after a failed purge left the participant off", async () => {
    serve({ connected: false });
    renderDetail(formerParty("failed", { purge_error: "decman restarted" }));

    fireEvent.click(await screen.findByRole("button", { name: "Reconnect" }));

    expect(
      await screen.findByText("The participant is connected and healthy again"),
    ).toBeTruthy();
    const posts = callsTo(RECONNECT_URL, "POST");
    expect(posts).toHaveLength(1);
    expect(JSON.parse(String(posts[0][1]?.body))).toEqual({ party_id: PARTY_ID });
  });

  it("shows why the server refused the reconnect", async () => {
    const reason =
      "Synchronizer 'global' is set to manual connection. DecMan sets that when it keeps the participant off on purpose";
    serve({ connected: false, reconnectStatus: 409, reconnectBody: { error: reason } });
    renderDetail(formerParty("failed"));

    fireEvent.click(await screen.findByRole("button", { name: "Reconnect" }));

    expect(await screen.findByText(reason)).toBeTruthy();
  });

  it("offers no reconnect while the participant is connected", async () => {
    serve({ connected: true });
    renderDetail(formerParty("failed"));
    await waitFor(() => expect(callsTo(HEALTH_URL)).toHaveLength(1));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(screen.queryByRole("button", { name: "Reconnect" })).toBeNull();
  });

  it("does not read the node's health unless a purge failed", () => {
    serve({ connected: false });
    renderDetail(formerParty("none"));
    expect(callsTo(HEALTH_URL)).toHaveLength(0);
    expect(screen.queryByRole("button", { name: "Reconnect" })).toBeNull();
  });
});
