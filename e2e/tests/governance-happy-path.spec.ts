import { test, expect, type Page } from "@playwright/test";
import {
  openParticipants, gotoTab, acceptInvitation, acceptInvitationOnAny, expectWorkflowCompleted,
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
    await dialog.getByLabel("Party ID Prefix").fill(prefix);
    if (opts.onlyOnePeer) {
      const boxes = dialog.getByRole("checkbox");
      const count = await boxes.count();
      for (let i = 1; i < count; i++) {
        if (await boxes.nth(i).isChecked()) await boxes.nth(i).uncheck();
      }
    }
    await dialog.getByRole("button", { name: "Start Onboarding" }).click();
  }

  test("01 two-member party (coordinator + exactly one peer)", async () => {
    const prefix = `e2e-two-${Date.now()}`;
    await startOnboarding(parts.p1, prefix, { onlyOnePeer: true });
    const peer = await acceptInvitationOnAny([parts.p2, parts.p3], /Onboarding/);
    await expectWorkflowCompleted(parts.p1, /Onboarding/);
    await expectWorkflowCompleted(peer, /Onboarding/);
    // Party visible on P1's Parties tab.
    await gotoTab(parts.p1, "Parties");
    await expect(parts.p1.getByText(new RegExp(prefix))).toBeVisible({ timeout: 60_000 });
    await test.info().attach(`Two-member party created: ${prefix}`, {
      body: await parts.p1.screenshot({ fullPage: true }), contentType: "image/png",
    });
  });

  test("02 create dec party (full mesh)", async () => {
    const prefix = `e2e-mesh-${Date.now()}`;
    shared.partyPrefix = prefix;
    await startOnboarding(parts.p1, prefix); // full mesh: all peers stay checked
    await acceptInvitation(parts.p2, /Onboarding/);
    await acceptInvitation(parts.p3, /Onboarding/);
    await expectWorkflowCompleted(parts.p1, /Onboarding/);
    await expectWorkflowCompleted(parts.p2, /Onboarding/);
    await expectWorkflowCompleted(parts.p3, /Onboarding/);
    await gotoTab(parts.p1, "Parties");
    const row = parts.p1.getByRole("row", { name: new RegExp(prefix) });
    await expect(row).toBeVisible({ timeout: 60_000 });
    await row.click(); // open PartyDetail to read the full party id
    shared.partyId = await parts.p1.getByText(/::1220[0-9a-f]+/i).first().innerText();
    await test.info().annotations.push({ type: "party id", description: shared.partyId });
  });
});
