import { test, expect, type Page } from "@playwright/test";
import {
  openParticipants, gotoTab, acceptInvitation, acceptInvitationOnAny, expectWorkflowCompleted,
  resolvePartyId, PORTS,
} from "../fixtures/participants.js";

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
});
