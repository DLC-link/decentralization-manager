import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import { authenticatedFetch } from "../api";
import { SnackbarProvider } from "../contexts";
import { ExternalPartyList } from "./ExternalPartyList";
import type {
  ExternalPartyInfo,
  ExternalPartyUnhostPreview,
  UnhostAuditEntry,
} from "../types";

vi.mock("../api", () => ({ authenticatedFetch: vi.fn() }));
vi.mock("../constants", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../constants")>()),
  ADMIN_ACCESS: true,
}));

const fetchMock = vi.mocked(authenticatedFetch);

const PARTY = "wallet::key";
const BASE = `/external-parties/${encodeURIComponent(PARTY)}`;
const id = (tag: string) => `${tag}${"0".repeat(60)}${tag}`;
/** How the dialog's id lists show an id: start and end around an ellipsis. */
const shown = (value: string) => `${value.slice(0, 24)}...${value.slice(-12)}`;

const PARTY_ROW: ExternalPartyInfo = {
  party_id: PARTY,
  fingerprint: "key",
  threshold: 2,
  host_count: 3,
  created_at: undefined,
  onboarding: false,
  hosts: [],
};

const preview = (extra: Partial<ExternalPartyUnhostPreview> = {}): ExternalPartyUnhostPreview => ({
  party_id: PARTY,
  serial: 4,
  threshold: 2,
  remaining_hosts: [id("p1"), id("p2")],
  validator_operator_party: "validator::ns",
  validator_rights: [id("vr")],
  preapprovals_to_cancel: [id("tp")],
  preapprovals_expired: [id("old")],
  blocker: undefined,
  ...extra,
});

const json = (body: unknown, status = 200) =>
  Promise.resolve(new Response(JSON.stringify(body), { status }));

const serve = (previewBody: ExternalPartyUnhostPreview, unhostStatus = 200) => {
  fetchMock.mockImplementation((input) => {
    const url = String(input);
    if (url === `${BASE}/unhost/preview`) return json(previewBody);
    if (url === `${BASE}/unhost`) {
      return json(
        {
          status: unhostStatus === 200 ? "completed" : "failed",
          party_id: PARTY,
          serial: 5,
          validator_rights_archived: [id("vr")],
          preapprovals_cancelled: [],
          audit_id: 1,
        },
        unhostStatus,
      );
    }
    return json({ error: `unexpected ${url}` }, 404);
  });
};

const unhostCalls = () =>
  fetchMock.mock.calls.filter(([input]) => String(input) === `${BASE}/unhost`);

const openStopHosting = async () => {
  render(<ExternalPartyList parties={[PARTY_ROW]} />, { wrapper: SnackbarProvider });
  fireEvent.click(screen.getByRole("button", { name: `Stop hosting ${PARTY}` }));
  await screen.findByText(/removes itself from the party's hosts/);
};

const confirmButton = () => screen.getByRole("button", { name: "Stop hosting" });

beforeEach(() => fetchMock.mockReset());

describe("Stop hosting an external party", () => {
  it("lists what it archives and sends nothing until the operator confirms", async () => {
    serve(preview());
    await openStopHosting();

    expect(screen.getByText(shown(id("vr")))).toBeTruthy();
    expect(screen.getByText(shown(id("tp")))).toBeTruthy();
    expect(screen.getByText(shown(id("old")))).toBeTruthy();
    expect(screen.getByText(shown(id("p1")))).toBeTruthy();
    expect(unhostCalls()).toHaveLength(0);
  });

  it("pins the previewed serial and leaves the cleanup on", async () => {
    serve(preview());
    await openStopHosting();

    fireEvent.click(confirmButton());

    await waitFor(() => expect(unhostCalls()).toHaveLength(1));
    const [, init] = unhostCalls()[0];
    expect(init?.method).toBe("POST");
    expect(JSON.parse(String(init?.body))).toEqual({ base_serial: 4 });
    expect(await screen.findByText(`This node no longer hosts ${PARTY}`)).toBeTruthy();
  });

  it("shows a blocker and keeps the button off", async () => {
    serve(preview({ blocker: "only 1 of the hosts that would remain can confirm" }));
    await openStopHosting();

    expect(screen.getByText(/only 1 of the hosts that would remain/)).toBeTruthy();
    expect((confirmButton() as HTMLButtonElement).disabled).toBe(true);
  });

  it("without an operator party, leaves only once the operator skips the cleanup", async () => {
    serve(preview({ validator_operator_party: undefined, validator_rights: [], preapprovals_to_cancel: [] }));
    await openStopHosting();

    expect((confirmButton() as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole("checkbox", { name: /never acted as the party's Splice validator/ }));
    fireEvent.click(confirmButton());

    await waitFor(() => expect(unhostCalls()).toHaveLength(1));
    const [, init] = unhostCalls()[0];
    expect(JSON.parse(String(init?.body))).toEqual({ base_serial: 4, splice_cleanup: false });
  });

  it("says the party is still hosted when a step fails", async () => {
    serve(preview(), 500);
    await openStopHosting();

    fireEvent.click(confirmButton());

    expect(await screen.findByText(/The party is still hosted here/)).toBeTruthy();
  });
});

describe("Unhost history", () => {
  const entry = (extra: Partial<UnhostAuditEntry>): UnhostAuditEntry => ({
    id: 1,
    party_id: PARTY,
    participant_id: "participant::ns",
    requested_via: "tenant",
    requested_by: "tenant-key-0",
    serial_before: 4,
    serial_after: undefined,
    validator_rights_archived: [],
    preapprovals_cancelled: [],
    splice_cleanup: true,
    step: "preconditions",
    resumed_from: undefined,
    started_at: 1_790_000_000,
    finished_at: 1_790_000_001,
    status: "failed",
    error: undefined,
    ...extra,
  });

  it("lists every attempt with its outcome, without opening the hosts panel", async () => {
    fetchMock.mockImplementation((input) =>
      String(input) === `${BASE}/unhost-audit`
        ? json({
            entries: [
              entry({ id: 2, status: "completed", step: "done", serial_after: 5, requested_via: "operator", requested_by: "ops@example.com" }),
              entry({ id: 1, error: "this host reads serial 4 for wallet::key, not the 3 the request pinned" }),
            ],
          })
        : json({ error: "unexpected" }, 404),
    );
    render(<ExternalPartyList parties={[PARTY_ROW]} />, { wrapper: SnackbarProvider });

    fireEvent.click(screen.getByRole("button", { name: `Unhost history of ${PARTY}` }));

    const rows = await screen.findAllByTestId("unhost-audit-row");
    expect(rows).toHaveLength(2);
    expect(rows[0].textContent).toContain("completed");
    expect(rows[0].textContent).toContain("4 → 5");
    expect(rows[0].textContent).toContain("ops@example.com");
    expect(rows[1].textContent).toContain("not the 3 the request pinned");
    // The open dialog hides the list from the accessibility tree.
    const row = screen.getByRole("button", {
      name: `Show the participants hosting ${PARTY}`,
      hidden: true,
    });
    expect(row.getAttribute("aria-expanded")).toBe("false");
  });
});
