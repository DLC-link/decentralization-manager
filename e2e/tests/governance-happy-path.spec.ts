import { test, expect, type Page } from "@playwright/test";
import {
  openParticipants, gotoTab, acceptInvitation, acceptInvitationOnAny, expectWorkflowCompleted,
  resolvePartyId, submitAndAwaitClose, PORTS,
  govRulesContractId, waitForProposalCid, govConfirm, waitForExecutable, govExecute,
} from "../fixtures/participants.js";
import { getAuthConfig, fetchRopcTokens } from "../fixtures/auth.js";

const PACKAGES = {
  governance_action: "#governance-action-v1",
  governance_core: "#governance-core-v1",
  governance_token_custody: "#governance-token-custody-v1",
  governance_utility_onboarding: "#governance-utility-onboarding-v1",
  utility_registry: "#utility-registry-app-v0",
};

function env(name: string): string {
  const v = process.env[name];
  if (!v) throw new Error(`required env var ${name} is not set (source integration-tests/devnet.env.sh)`);
  return v;
}

// Mirrors deploy_gov_core.rs update_party_config + grant_rights_devnet, but via
// the same HTTP endpoints the PartyConfigDialog / grant-rights flow call.
async function configurePartyForGovCore(partyId: string) {
  const nodes = [
    { port: 8081, n: "1" },
    { port: 8082, n: "2" },
    { port: 8083, n: "3" },
  ];
  for (const { port, n } of nodes) {
    const cfg = await getAuthConfig(port);
    const { access_token } = await fetchRopcTokens(cfg);
    const authHeaders = { authorization: `Bearer ${access_token}`, "content-type": "application/json" };

    // PUT /party-config
    const cfgRes = await fetch(`http://localhost:${port}/party-config`, {
      method: "PUT",
      headers: authHeaders,
      body: JSON.stringify({
        dec_party_id: partyId,
        member_party_id: env(`P${n}_MEMBER_PARTY_ID`),
        user_id: env(`P${n}_MEMBER_USER_ID`),
        keycloak_url: env("DECPM_KEYCLOAK_URL"),
        keycloak_realm: env("DECPM_KEYCLOAK_REALM"),
        keycloak_client_id: env(`P${n}_MEMBER_KEYCLOAK_CLIENT_ID`),
        keycloak_client_secret: env(`P${n}_MEMBER_KEYCLOAK_CLIENT_SECRET`),
        packages: PACKAGES,
      }),
    });
    if (!cfgRes.ok) throw new Error(`PUT /party-config on :${port} → ${cfgRes.status}: ${await cfgRes.text()}`);
  }
  // grant-rights must run AFTER all party-configs are persisted (IT comment).
  for (const { port, n } of nodes) {
    const cfg = await getAuthConfig(port);
    const { access_token } = await fetchRopcTokens(cfg);
    const grRes = await fetch(`http://localhost:${port}/auth/grant-rights`, {
      method: "POST",
      headers: { authorization: `Bearer ${access_token}`, "content-type": "application/json" },
      body: JSON.stringify({
        dec_party_id: partyId,
        admin_client_id: env(`P${n}_PARTICIPANT_ADMIN_KEYCLOAK_CLIENT_ID`),
        admin_client_secret: env(`P${n}_PARTICIPANT_ADMIN_KEYCLOAK_CLIENT_SECRET`),
      }),
    });
    if (!grRes.ok) throw new Error(`POST /auth/grant-rights on :${port} → ${grRes.status}: ${await grRes.text()}`);
  }
}

test.describe.serial("governance happy path", () => {
  let parts: Awaited<ReturnType<typeof openParticipants>>;
  const shared: { partyPrefix?: string; partyId?: string } = {};

  test.beforeAll(async ({ browser }) => {
    parts = await openParticipants(browser);
  });
  test.afterAll(async () => { await parts?.closeAll(); });

  // Onboarding peer checkboxes default to all-checked. `onlyOnePeer` leaves
  // exactly one checked → a strict subset (the two_member_party regression).
  // Peer labels are `peer.name || peer.participant_id` (OnboardingDialog.tsx:356),
  // which don't map reliably to P2/P3, so we keep the first listed peer and
  // accept on whichever peer context actually receives the invitation.
  async function startOnboarding(
    coordinator: Page, prefix: string, opts: { onlyOnePeer?: boolean } = {},
  ) {
    await gotoTab(coordinator, "Parties");
    await coordinator.getByRole("button", { name: "Create Party" }).click();
    const dialog = coordinator.getByRole("dialog");
    await expect(dialog.getByText("Create Decentralized Party")).toBeVisible();
    await dialog.getByPlaceholder("e.g., my-network").fill(prefix);
    // Wait for the peer list to finish loading (checkboxes render + default
    // selection applied) before touching or submitting — otherwise the full-mesh
    // path can click Start before peers are selected.
    const boxes = dialog.getByRole("checkbox");
    await expect(boxes.first()).toBeVisible({ timeout: 30_000 });
    if (opts.onlyOnePeer) {
      const count = await boxes.count();
      for (let i = 1; i < count; i++) {
        if (await boxes.nth(i).isChecked()) await boxes.nth(i).uncheck();
      }
    }
    // On success the handler closes the dialog (OnboardingDialog.tsx:221); retry the
    // transient back-to-back 409 until it starts.
    await submitAndAwaitClose(dialog, "Start Onboarding");
  }

  test("01 two-member party (coordinator + exactly one peer)", async () => {
    const prefix = `e2e-two-${Date.now()}`;
    await startOnboarding(parts.p1, prefix, { onlyOnePeer: true });
    const peer = await acceptInvitationOnAny([parts.p2, parts.p3], /Onboarding/);
    // Success = this run's Onboarding workflow card shows completed on the
    // coordinator and the one invited peer (prefix-scoped, so a stale completed
    // card can't satisfy it; immediate, unlike the lagging Parties list).
    await expectWorkflowCompleted(parts.p1, prefix);
    await expectWorkflowCompleted(peer, prefix);
    await test.info().attach(`Two-member onboarding completed: ${prefix}`, {
      body: await parts.p1.screenshot({ fullPage: true }), contentType: "image/png",
    });
  });

  test("02 create dec party (full mesh)", async () => {
    const prefix = `e2e-mesh-${Date.now()}`;
    shared.partyPrefix = prefix;
    await startOnboarding(parts.p1, prefix); // full mesh: all peers stay checked
    await acceptInvitation(parts.p2, /Onboarding/);
    await acceptInvitation(parts.p3, /Onboarding/);
    await expectWorkflowCompleted(parts.p1, prefix);
    await expectWorkflowCompleted(parts.p2, prefix);
    await expectWorkflowCompleted(parts.p3, prefix);
    // Capture the full party id for downstream phases (prefix-scoped refresh —
    // the UI Parties list lags without a forced refresh).
    shared.partyId = await resolvePartyId(PORTS.p1, prefix);
    test.info().annotations.push({ type: "party id", description: shared.partyId });
    await test.info().attach(`Full-mesh party created: ${shared.partyId}`, {
      body: await parts.p1.screenshot({ fullPage: true }), contentType: "image/png",
    });
  });

  test("03 distribute DARs", async () => {
    // DARs are committed under releases/v0/release (repo root); cwd is e2e/.
    const darDir = "../releases/v0/release";
    const dars = [
      "governance-action-v1-0.1.0.dar",
      "governance-core-v1-0.1.0.dar",
      "governance-token-custody-v1-0.1.0.dar",
      "governance-utility-onboarding-v1-0.1.0.dar",
    ].map((f) => `${darDir}/${f}`);

    await gotoTab(parts.p1, "Packages");
    await parts.p1.getByRole("button", { name: "Upload DARs" }).click();
    let dialog = parts.p1.getByRole("dialog");
    await dialog.locator('input[type="file"]').setInputFiles(dars);
    await dialog.getByRole("button", { name: "Upload DARs" }).click();
    await expect(dialog.getByText(/uploaded to this node successfully/i)).toBeVisible({ timeout: 120_000 });
    await dialog.getByRole("button", { name: "Close" }).click();

    await parts.p1.getByRole("button", { name: "Distribute DARs" }).click();
    dialog = parts.p1.getByRole("dialog");
    await dialog.locator('input[type="file"]').setInputFiles(dars);
    // Distribute, like onboarding, starts the workflow and closes the dialog
    // (DarsDialog.tsx:231) — and can transiently 409 right after onboarding, so
    // retry. Peers then accept; completion is verified by phase 04's comparison.
    await submitAndAwaitClose(dialog, "Distribute DARs");
    await acceptInvitation(parts.p2, /Dars/);
    await acceptInvitation(parts.p3, /Dars/);
  });

  test("04 check peer DARs", async () => {
    await gotoTab(parts.p1, "Packages");
    await parts.p1.getByRole("button", { name: "Check Peer DARs" }).click();
    // Comparison table renders peer columns; matches show a success (CheckCircle) icon.
    // Assert the governance-core package row shows matches for both peers (no error/"-").
    await expect(parts.p1.getByText(/governance-core/i).first()).toBeVisible({ timeout: 60_000 });
    await expect(parts.p1.getByText(/^-$/)).toHaveCount(0); // no "unreachable" cells
    await test.info().attach("Peer DAR comparison (P1 sees P2 + P3)", {
      body: await parts.p1.screenshot({ fullPage: true }), contentType: "image/png",
    });
  });

  // Filter the (100+ party) list by full prefix and open that party's detail.
  // Navigate to the app root first so a PartyDetail left open by a prior phase
  // is cleared — the "Filter by full prefix" box only exists on the list view.
  async function openParty(page: Page, prefix: string) {
    await page.goto(new URL("/", page.url()).href);
    await gotoTab(page, "Parties");
    const filter = page.getByPlaceholder(/Filter by full prefix/i);
    await expect(filter).toBeVisible({ timeout: 30_000 });
    await filter.fill(prefix);
    await filter.press("Enter");
    await page.getByRole("row", { name: new RegExp(prefix) }).click();
  }

  test("05 deploy governance core", async () => {
    // Falls back to env vars so this phase can be iterated standalone against an
    // already-onboarded party (E2E_PARTY_ID + E2E_PARTY_PREFIX).
    const partyId = shared.partyId ?? process.env.E2E_PARTY_ID;
    const partyPrefix = shared.partyPrefix ?? process.env.E2E_PARTY_PREFIX;
    test.skip(!partyId || !partyPrefix, "no party (run phase 02, or set E2E_PARTY_ID + E2E_PARTY_PREFIX)");

    // (a+b) Setup: party-config + grant-rights on all 3 nodes (API, mirrors IT).
    await configurePartyForGovCore(partyId!);

    // (c) Deploy Governance Core via the UI (ContractsDialog). The gov-core card
    //     is enabled because the governance-core DAR was distributed in phase 03.
    await openParty(parts.p1, partyPrefix!);
    await parts.p1.getByRole("button", { name: "Deploy Contracts" }).click();
    const dialog = parts.p1.getByRole("dialog");
    await dialog.getByText("Governance Core").click();        // the gov-core card
    // Deploy starts the Contracts workflow and closes the dialog; retry the
    // transient back-to-back 409 until it starts.
    await submitAndAwaitClose(dialog, "Deploy Contracts");

    await acceptInvitation(parts.p2, /Contracts/);
    await acceptInvitation(parts.p3, /Contracts/);
    await expectWorkflowCompleted(parts.p1, partyPrefix!);

    // Gov-core rules now exist → PartyDetail exposes "New Proposal" (IT's
    // GovernanceRules-visible assertion, expressed through the UI; needed by phase 06).
    // The detail view can lag the just-created GovernanceRules contract, so reload +
    // re-open until it surfaces.
    await expect(async () => {
      await parts.p1.reload();
      await openParty(parts.p1, partyPrefix!);
      await expect(parts.p1.getByRole("button", { name: /New Proposal/i })).toBeVisible({ timeout: 10_000 });
    }).toPass({ timeout: 120_000, intervals: [5000, 5000, 10_000] });
  });

  // Hybrid by necessity: only the *propose* step runs through the UI. A single
  // shared frontend user drives all 3 nodes, so the UI treats the proposer's
  // confirmation as "yours" on every node and never offers the peer Confirm /
  // Execute dialogs — those are reached via the per-node /governance/* API
  // (as the Rust IT does). The Confirm/Execute UI dialogs are a known coverage
  // gap (see e2e/README.md); covering them needs distinct per-node users.
  test("06 generic vote — propose (UI) + reach threshold (API)", async () => {
    // Falls back to env vars so this phase can be iterated standalone against an
    // already-deployed party (E2E_PARTY_ID + E2E_PARTY_PREFIX).
    const partyId = shared.partyId ?? process.env.E2E_PARTY_ID;
    const partyPrefix = shared.partyPrefix ?? process.env.E2E_PARTY_PREFIX;
    test.skip(!partyId || !partyPrefix, "no party (run phase 05, or set E2E_PARTY_ID + E2E_PARTY_PREFIX)");

    // --- Propose via UI (P1) ---
    await openParty(parts.p1, partyPrefix!);
    await parts.p1.getByRole("button", { name: /New Proposal/i }).click();
    const dialog = parts.p1.getByRole("dialog");
    // Proposal Type is the only Select in this dialog; getByLabel doesn't resolve
    // a MUI Select, so target the combobox role. Options render in a portal, so
    // select them at page level.
    await dialog.getByRole("combobox").click();
    await parts.p1.getByRole("option", { name: /generic vote/i }).click();
    const desc = `e2e dark theme ${Date.now()}`;
    // "Vote Description" label collides with the help-icon aria-label under
    // getByLabel, so target the textbox role.
    await dialog.getByRole("textbox", { name: /Vote Description/i }).fill(desc);
    await parts.p1.getByRole("button", { name: /Submit Proposal/i }).click();

    // Assert the proposal landed, via the UI feed (the vote description appears).
    await gotoTab(parts.p1, "Pending approvals");
    await expect(parts.p1.getByText(desc)).toBeVisible({ timeout: 90_000 });

    // --- Confirm (P2) + execute (P3) via API ---
    // All 3 nodes share one frontend user, so the UI treats the proposer's
    // confirmation as "yours" on every node and offers no second Confirm. We
    // reach threshold via the per-node /governance/* endpoints (as the Rust IT
    // does), then verify the result back in the UI.
    const rulesCid = await govRulesContractId(PORTS.p1, partyId!);
    const proposalCid = await waitForProposalCid(PORTS.p1, partyId!);
    await govConfirm(PORTS.p2, partyId!, rulesCid, proposalCid);
    const confirmationCids = await waitForExecutable(PORTS.p3, partyId!, proposalCid);
    await govExecute(PORTS.p3, partyId!, rulesCid, proposalCid, confirmationCids);

    // --- Assert via UI: the action is gone from P1's feed ---
    await expect(async () => {
      await parts.p1.reload();
      await gotoTab(parts.p1, "Pending approvals");
      await expect(parts.p1.getByText(desc)).toHaveCount(0);
    }).toPass({ timeout: 120_000, intervals: [3000, 5000, 5000] });
  });

  test("07 notification feed: dismiss a completed run", async () => {
    await gotoTab(parts.p1, "Pending approvals");
    await parts.p1.reload();
    await gotoTab(parts.p1, "Pending approvals");
    // A completed Onboarding run from phase 02 should be present and dismissable.
    const dismiss = parts.p1.getByRole("button", { name: "Dismiss" }).first();
    await expect(dismiss).toBeVisible({ timeout: 60_000 });
    const before = await parts.p1.getByRole("button", { name: "Dismiss" }).count();
    await dismiss.click();
    await expect(async () => {
      await parts.p1.reload();
      await gotoTab(parts.p1, "Pending approvals");
      const after = await parts.p1.getByRole("button", { name: "Dismiss" }).count();
      expect(after).toBeLessThan(before);
    }).toPass({ timeout: 30_000 });
  });

  test("08 kick participant P3", async () => {
    const partyId = shared.partyId ?? process.env.E2E_PARTY_ID;
    const partyPrefix = shared.partyPrefix ?? process.env.E2E_PARTY_PREFIX;
    test.skip(!partyId || !partyPrefix, "no party (run phase 02, or set E2E_PARTY_ID + E2E_PARTY_PREFIX)");

    await openParty(parts.p1, partyPrefix!);
    // Participants section is expanded by default. The kick control is an
    // icon button (aria-label "Kick participant"; self is "Cannot kick
    // yourself" and disabled). Kick the last non-self participant.
    const kickBtn = parts.p1.getByRole("button", { name: "Kick participant" }).last();
    await expect(kickBtn).toBeVisible({ timeout: 30_000 });
    await kickBtn.click();
    const dialog = parts.p1.getByRole("dialog");
    await expect(dialog.getByRole("heading", { name: /Kick Participant/i })).toBeVisible();
    await dialog.getByRole("spinbutton").fill("2"); // New Threshold (number field)
    // Submit is disabled until the owner key resolves; submitAndAwaitClose waits
    // for it to enable and retries the transient 409.
    await submitAndAwaitClose(dialog, /Kick Participant/i);

    await acceptInvitation(parts.p2, /Kick/);
    await expectWorkflowCompleted(parts.p1, partyPrefix!);

    // Party now has 2 participants (the kicked one is gone). Topology lags, so
    // reopen + poll until the participant list shows 2.
    await expect(async () => {
      await openParty(parts.p1, partyPrefix!);
      await expect(parts.p1.getByText(/iBTC-validator-\d/)).toHaveCount(2);
    }).toPass({ timeout: 120_000, intervals: [5000, 5000, 10_000] });
    await test.info().attach("After kick: party has 2 participants", {
      body: await parts.p1.screenshot({ fullPage: true }), contentType: "image/png",
    });
  });
});
