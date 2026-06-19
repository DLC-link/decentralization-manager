import { expect, type Browser, type Page } from "@playwright/test";
import { getAuthConfig, fetchRopcTokens, seedAuth } from "./auth.js";

export const PORTS = { p1: 8081, p2: 8082, p3: 8083 } as const;

async function openAuthed(browser: Browser, port: number): Promise<Page> {
  const cfg = await getAuthConfig(port);
  const context = await browser.newContext();
  if (cfg.auth_required) {
    const tokens = await fetchRopcTokens(cfg);
    await seedAuth(context, tokens, cfg);
  }
  const page = await context.newPage();
  await page.goto(`http://localhost:${port}/`);
  // Authed shell renders the Sidebar nav (the Logout button is icon-only with
  // no accessible name, so we key off the "Parties" nav item instead).
  await expect(page.getByRole("button", { name: /Parties/ })).toBeVisible();
  return page;
}

export async function openParticipants(browser: Browser) {
  const p1 = await openAuthed(browser, PORTS.p1);
  const p2 = await openAuthed(browser, PORTS.p2);
  const p3 = await openAuthed(browser, PORTS.p3);
  const closeAll = async () => {
    for (const p of [p1, p2, p3]) await p.context().close();
  };
  return { p1, p2, p3, closeAll };
}

// Sidebar/Header navigation. Tab labels per Sidebar.tsx: "Parties",
// "Packages", "Pending approvals".
export async function gotoTab(page: Page, tab: string) {
  await page.getByRole("button", { name: tab }).first().click();
}

// The Pending-approvals feed auto-polls (/invitations every ~2s, /workflows for
// run status), so we navigate to the tab ONCE and let Playwright's auto-waiting
// ride the app's polling — reloading would reset to the Parties tab and wipe the
// just-fetched feed before it populates.
export async function acceptInvitation(page: Page, type: RegExp) {
  await gotoTab(page, "Pending approvals");
  const label = new RegExp(`${type.source} invitation`, "i");
  await expect(page.getByText(label).first()).toBeVisible({ timeout: 90_000 });
  await page.getByRole("button", { name: "Accept" }).first().click();
}

export async function acceptInvitationOnAny(pages: Page[], type: RegExp): Promise<Page> {
  const label = new RegExp(`${type.source} invitation`, "i");
  for (const p of pages) await gotoTab(p, "Pending approvals");
  let target: Page | undefined;
  await expect(async () => {
    for (const p of pages) {
      if (await p.getByText(label).count()) { target = p; return; }
    }
    throw new Error("invitation not yet visible on any peer");
  }).toPass({ timeout: 90_000, intervals: [2000, 2000, 3000] });
  await target!.getByRole("button", { name: "Accept" }).first().click();
  return target!;
}

// Assert the workflow for THIS run completed, scoped by its prefix. The
// WorkflowRunCard shows a prefix chip + a "completed" status chip; we match a
// feed card containing BOTH so a stale completed run from an earlier prefix
// can't satisfy it, and the feed's own /workflows polling flips it to completed
// without a reload.
export async function expectWorkflowCompleted(page: Page, _kind: RegExp, prefix: string) {
  await gotoTab(page, "Pending approvals");
  const card = page.locator("div").filter({ hasText: prefix }).filter({ hasText: /completed/i });
  await expect(card.first()).toBeVisible({ timeout: 240_000 });
}

// Resolve the full party id (<prefix>::1220…) for a freshly-onboarded party.
// Uses the prefix-scoped refresh lookup (the UI's plain prefix filter does NOT
// force a refresh, so a brand-new party lags there). Used to thread state to
// downstream phases.
export async function resolvePartyId(port: number, prefix: string): Promise<string> {
  const cfg = await getAuthConfig(port);
  const deadline = Date.now() + 90_000;
  let last = "no response";
  // A freshly-onboarded party can lag the topology view briefly even with
  // refresh=true, so poll until it surfaces.
  while (Date.now() < deadline) {
    const { access_token } = await fetchRopcTokens(cfg);
    const res = await fetch(
      `http://localhost:${port}/decentralized-parties?prefix=${encodeURIComponent(prefix)}&refresh=true`,
      { headers: { authorization: `Bearer ${access_token}` } },
    );
    if (res.ok) {
      const data = await res.json();
      const party = (data.parties ?? []).find((p: { party_id?: string }) =>
        (p.party_id ?? "").startsWith(`${prefix}::`),
      );
      if (party) return party.party_id as string;
      last = "not in topology yet";
    } else {
      last = `status ${res.status}`;
    }
    await new Promise((r) => setTimeout(r, 3000));
  }
  throw new Error(`party with prefix ${prefix} not found on :${port} after 90s (${last})`);
}
