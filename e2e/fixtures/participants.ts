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

export async function acceptInvitation(page: Page, type: RegExp) {
  await gotoTab(page, "Pending approvals");
  // The Accept button lives in the same invitation card.
  await expect(async () => {
    await gotoTab(page, "Pending approvals");
    await expect(page.getByText(new RegExp(`${type.source} invitation`, "i"))).toBeVisible();
  }).toPass({ timeout: 60_000 });
  await page.getByRole("button", { name: "Accept" }).first().click();
}

export async function acceptInvitationOnAny(pages: Page[], type: RegExp): Promise<Page> {
  let target: Page | undefined;
  const label = new RegExp(`${type.source} invitation`, "i");
  await expect(async () => {
    for (const p of pages) {
      await gotoTab(p, "Pending approvals");
      await p.reload();
      await gotoTab(p, "Pending approvals");
      if (await p.getByText(label).count()) { target = p; return; }
    }
    throw new Error("invitation not yet visible on any peer");
  }).toPass({ timeout: 60_000 });
  await target!.getByRole("button", { name: "Accept" }).first().click();
  return target!;
}

export async function expectWorkflowCompleted(page: Page, kind: RegExp) {
  await expect(async () => {
    await gotoTab(page, "Pending approvals");
    await page.reload();
    await gotoTab(page, "Pending approvals");
    const card = page.getByText(new RegExp(`${kind.source} workflow`, "i")).first();
    await expect(card).toBeVisible();
    // status chip text within the same feed; "completed" rendering per WorkflowRunCard.
    await expect(page.getByText(/completed/i).first()).toBeVisible();
  }).toPass({ timeout: 240_000 });
}
