import { expect, type Browser, type Page, type Locator } from "@playwright/test";
import { getAuthConfig, fetchRopcTokens, seedAuth } from "./auth.js";

// --- Shared polling / fetch helpers ----------------------------------------

/** Poll `fn` until it returns a value, or throw naming `label` and the last
 *  reason it did not. `fn` returns `undefined` to keep polling; it may also
 *  report *why* by writing to `reason`, which the timeout message quotes.
 *  Replaces three near-identical hand-rolled loops. */
export async function pollUntil<T>(
  label: string,
  fn: (reason: (why: string) => void) => Promise<T | undefined>,
  opts: { timeoutMs?: number; intervalMs?: number } = {},
): Promise<T> {
  const timeoutMs = opts.timeoutMs ?? 90_000;
  const intervalMs = opts.intervalMs ?? 3000;
  const deadline = Date.now() + timeoutMs;
  let last = "no attempt completed";
  const reason = (why: string) => { last = why; };
  while (Date.now() < deadline) {
    try {
      const got = await fn(reason);
      if (got !== undefined) return got;
    } catch (e) {
      last = String(e);
    }
    await new Promise((r) => setTimeout(r, intervalMs));
  }
  throw new Error(`${label} after ${Math.round(timeoutMs / 1000)}s (${last})`);
}

/** Bearer-authenticated fetch against a node. Pass `token` to reuse one across
 *  a poll; omit it and a fresh ROPC token is minted per call. Throws on a
 *  non-2xx, with the status and body, so no caller repeats that check. */
export async function authedFetch(
  port: number,
  path: string,
  init: RequestInit & { token?: string } = {},
): Promise<Response> {
  const { token, headers, ...rest } = init;
  const tok = token ?? (await bearer(port));
  const res = await fetch(`http://localhost:${port}${path}`, {
    ...rest,
    headers: { ...(headers ?? {}), authorization: `Bearer ${tok}` },
  });
  if (!res.ok) {
    throw new Error(`${path} on :${port} → ${res.status}: ${await res.text()}`);
  }
  return res;
}

// Click a workflow-start submit button (in a dialog that closes on success),
// retrying the transient "Another workflow is already running" 409 — the
// coordinator allows one in-flight workflow at a time and the guard releases
// just after the prior run's card flips to completed. A persistent error (e.g.
// a real mesh hole / bad config) keeps the dialog open and surfaces on timeout.
export async function submitAndAwaitClose(dialog: Locator, submitName: string | RegExp) {
  const btn = dialog.getByRole("button", { name: submitName });
  await expect(btn).toBeEnabled({ timeout: 15_000 });
  await expect(async () => {
    if (await dialog.isHidden()) return;
    await btn.click();
    await expect(dialog).toBeHidden({ timeout: 8_000 });
  }).toPass({ timeout: 90_000, intervals: [3000, 3000, 5000] });
}

export const PORTS = { p1: 8081, p2: 8082, p3: 8083 } as const;

async function openAuthed(browser: Browser, port: number): Promise<Page> {
  const cfg = await getAuthConfig(port);
  const context = await browser.newContext();
  if (cfg.auth_required) {
    const tokens = await fetchRopcTokens(cfg);
    await seedAuth(context, tokens);
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
// Find the invitation card for THIS workflow type (data-testid="invitation-card",
// scoped by its "<type> invitation" heading) and click Accept WITHIN it — never
// the first Accept on the page, which could belong to a different/lingering card.
function invitationCard(page: Page, type: RegExp): Locator {
  const label = new RegExp(`${type.source} invitation`, "i");
  return page.getByTestId("invitation-card").filter({ hasText: label });
}

export async function acceptInvitation(page: Page, type: RegExp) {
  await gotoTab(page, "Pending approvals");
  const card = invitationCard(page, type);
  await expect(card.first()).toBeVisible({ timeout: 90_000 });
  await card.first().getByRole("button", { name: "Accept" }).click();
}

export async function acceptInvitationOnAny(pages: Page[], type: RegExp): Promise<Page> {
  for (const p of pages) await gotoTab(p, "Pending approvals");
  let target: Page | undefined;
  await expect(async () => {
    for (const p of pages) {
      if (await invitationCard(p, type).count()) { target = p; return; }
    }
    throw new Error("invitation not yet visible on any peer");
  }).toPass({ timeout: 90_000, intervals: [2000, 2000, 3000] });
  await invitationCard(target!, type).first().getByRole("button", { name: "Accept" }).click();
  return target!;
}

// Assert the workflow for THIS run completed. Matches the single WorkflowRunCard
// ELEMENT (data-testid="workflow-run-card") whose data-status is "completed" and
// whose data-prefix is this run's prefix — or, for runs that aren't prefix-tagged
// (Dars), whose data-kind matches. Selecting the card element (not an ancestor
// div) avoids the false-positive where a sibling completed card under a common
// ancestor satisfied a loose text filter. The feed's /workflows polling flips
// data-status to "completed" without a reload.
export async function expectWorkflowCompleted(page: Page, prefix: string, kind?: string) {
  await gotoTab(page, "Pending approvals");
  const selector = kind
    ? `[data-testid="workflow-run-card"][data-kind="${kind}"][data-status="completed"]`
    : `[data-testid="workflow-run-card"][data-prefix="${prefix}"][data-status="completed"]`;
  await expect(page.locator(selector).first()).toBeVisible({ timeout: 240_000 });
}

// Resolve the full party id (<prefix>::1220…) for a freshly-onboarded party.
// Uses the prefix-scoped refresh lookup (the UI's plain prefix filter does NOT
// force a refresh, so a brand-new party lags there). Used to thread state to
// downstream phases.
export async function resolvePartyId(port: number, prefix: string): Promise<string> {
  // One token for the whole poll (ROPC tokens last minutes) — avoids ~30
  // password-grant requests at Keycloak over a 90s poll.
  const token = await bearer(port);
  // A freshly-onboarded party can lag the topology view briefly even with
  // refresh=true, so poll until it surfaces.
  return pollUntil(`party with prefix ${prefix} not found on :${port}`, async (reason) => {
    const res = await authedFetch(
      port,
      `/decentralized-parties?prefix=${encodeURIComponent(prefix)}&refresh=true`,
      { token },
    );
    const data = await res.json();
    const party = (data.parties ?? []).find((p: { party_id?: string }) =>
      (p.party_id ?? "").startsWith(`${prefix}::`),
    );
    if (party) return party.party_id as string;
    reason("not in topology yet");
    return undefined;
  });
}

// --- Governance hybrid helpers ---------------------------------------------
// A single shared frontend user (cvault-finoa-lp-1) drives all 3 nodes, so the
// UI's "you already confirmed" logic blocks a second distinct confirmation
// through the UI. These mirror the Rust IT's per-node /governance/* calls so
// the propose→confirm→execute cycle can complete; the UI still drives the
// propose and asserts the result. (Endpoint payloads mirror tests/common/governance.rs.)

async function bearer(port: number): Promise<string> {
  const cfg = await getAuthConfig(port);
  const { access_token } = await fetchRopcTokens(cfg);
  return access_token;
}

interface DomainAction { proposal_cid: string; can_execute?: boolean; confirmations?: { contract_id: string }[]; }

async function govConfirmations(port: number, partyId: string, token?: string): Promise<DomainAction[]> {
  const res = await authedFetch(
    port,
    `/governance/confirmations?party_id=${encodeURIComponent(partyId)}`,
    { token },
  );
  return (await res.json()).domain_actions ?? [];
}

// GovernanceRules contract id for a party (from /governance/state).
export async function govRulesContractId(port: number, partyId: string): Promise<string> {
  const res = await authedFetch(port, `/governance/state?party_id=${encodeURIComponent(partyId)}`);
  const cid = (await res.json())?.state?.contract_id;
  if (!cid) throw new Error(`no GovernanceRules contract for ${partyId} on :${port}`);
  return cid;
}

// Poll until EXACTLY one pending domain action exists; return its proposal_cid.
// Requiring exactly one (matching the Rust IT's `domain_actions.len() == 1`
// wait in tests/common/governance.rs) means a lingering/extra proposal fails
// loudly here instead of silently confirming/executing the wrong CID.
export async function waitForProposalCid(port: number, partyId: string): Promise<string> {
  const tok = await bearer(port); // one token for the whole poll
  return pollUntil(`expected exactly one pending proposal on :${port}`, async (reason) => {
    const actions = await govConfirmations(port, partyId, tok);
    if (actions.length === 1 && actions[0].proposal_cid) return actions[0].proposal_cid;
    reason(`${actions.length} actions`);
    return undefined;
  });
}

// `action` is required by the /governance/{confirm,execute} request schema but
// is IGNORED on the core_domain path — the backend derives the Daml choice arg
// from proposal_cid (+ confirmer/executor + confirmation_cids), so this is a
// placeholder, not "confirm a set-threshold". Mirrors the Rust IT, which
// hardcodes the same action in confirm/execute (tests/common/governance.rs).
const THRESHOLD_ACTION = { type: "governance_set_threshold", new_threshold: 1 };

// Confirm the proposal as this node's member (mirrors IT propose_confirm_execute).
export async function govConfirm(port: number, partyId: string, rulesCid: string, proposalCid: string) {
  await authedFetch(port, "/governance/confirm", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      party_id: partyId, rules_contract_id: rulesCid,
      action: THRESHOLD_ACTION, governance_type: "core_domain", proposal_cid: proposalCid,
    }),
  });
}

// Poll until the proposal is executable; return its confirmation contract ids.
export async function waitForExecutable(port: number, partyId: string, proposalCid: string): Promise<string[]> {
  const tok = await bearer(port); // one token for the whole poll
  return pollUntil(`proposal ${proposalCid} not executable on :${port}`, async (reason) => {
    const a = (await govConfirmations(port, partyId, tok)).find((x) => x.proposal_cid === proposalCid && x.can_execute);
    if (a) return (a.confirmations ?? []).map((c) => c.contract_id);
    reason("not executable yet");
    return undefined;
  });
}

export async function govExecute(port: number, partyId: string, rulesCid: string, proposalCid: string, confirmationCids: string[]) {
  await authedFetch(port, "/governance/execute", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({
      party_id: partyId, rules_contract_id: rulesCid, action: THRESHOLD_ACTION,
      confirmation_cids: confirmationCids, disclosed_contracts: [],
      governance_type: "core_domain", proposal_cid: proposalCid,
    }),
  });
}
