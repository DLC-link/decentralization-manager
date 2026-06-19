import { test, expect, type Page } from "@playwright/test";
import {
  openParticipants, gotoTab, acceptInvitation, acceptInvitationOnAny, expectWorkflowCompleted,
  resolvePartyId, PORTS,
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
    const start = dialog.getByRole("button", { name: "Start Onboarding" });
    await expect(start).toBeEnabled({ timeout: 15_000 });
    // On success the handler closes the dialog (OnboardingDialog.tsx:221). Back-to-back
    // onboardings can transiently 409 ("Another workflow is already running…") because the
    // coordinator's in-flight guard releases just after the prior run's card shows completed;
    // re-clicking Start re-submits once the guard frees. A persistent error (e.g. a real mesh
    // hole) keeps failing and surfaces when this times out.
    await expect(async () => {
      if (await dialog.isHidden()) return;
      await start.click();
      await expect(dialog).toBeHidden({ timeout: 8_000 });
    }).toPass({ timeout: 90_000, intervals: [3000, 3000, 5000] });
  }

  test("01 two-member party (coordinator + exactly one peer)", async () => {
    const prefix = `e2e-two-${Date.now()}`;
    await startOnboarding(parts.p1, prefix, { onlyOnePeer: true });
    const peer = await acceptInvitationOnAny([parts.p2, parts.p3], /Onboarding/);
    // Success = this run's Onboarding workflow card shows completed on the
    // coordinator and the one invited peer (prefix-scoped, so a stale completed
    // card can't satisfy it; immediate, unlike the lagging Parties list).
    await expectWorkflowCompleted(parts.p1, /Onboarding/, prefix);
    await expectWorkflowCompleted(peer, /Onboarding/, prefix);
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
    await expectWorkflowCompleted(parts.p1, /Onboarding/, prefix);
    await expectWorkflowCompleted(parts.p2, /Onboarding/, prefix);
    await expectWorkflowCompleted(parts.p3, /Onboarding/, prefix);
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
    await dialog.getByRole("button", { name: "Distribute DARs" }).click();
    // Distribute, like onboarding, starts the workflow and closes the dialog
    // (DarsDialog.tsx:231). Peers then accept; completion (DARs vetted on peers)
    // is verified by the peer-DAR comparison in phase 04.
    await expect(dialog).toBeHidden({ timeout: 30_000 });
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

  test("05 deploy governance core", async () => {
    test.skip(!shared.partyId, "no party from phase 02");

    // (a+b) Setup: party-config + grant-rights on all 3 nodes (API, mirrors IT).
    await configurePartyForGovCore(shared.partyId!);

    // (c) Deploy Governance Core via the UI (ContractsDialog). The gov-core card
    //     is enabled because the governance-core DAR was distributed in phase 03.
    await gotoTab(parts.p1, "Parties");
    await parts.p1.getByRole("row", { name: new RegExp(shared.partyPrefix!) }).click();
    await parts.p1.getByRole("button", { name: "Deploy Contracts" }).click();
    const dialog = parts.p1.getByRole("dialog");
    await dialog.getByText("Governance Core").click();        // the gov-core card
    await dialog.getByRole("button", { name: "Deploy Contracts" }).click();

    await acceptInvitation(parts.p2, /Contracts/);
    await acceptInvitation(parts.p3, /Contracts/);
    await expectWorkflowCompleted(parts.p1, /Contracts/, shared.partyPrefix!);

    // Gov-core rules now exist → PartyDetail exposes "New Proposal" (IT's
    // GovernanceRules-visible assertion, expressed through the UI). Needed by phase 06.
    await gotoTab(parts.p1, "Parties");
    await parts.p1.getByRole("row", { name: new RegExp(shared.partyPrefix!) }).click();
    await expect(parts.p1.getByRole("button", { name: /New Proposal/i })).toBeVisible({ timeout: 60_000 });
  });
});
