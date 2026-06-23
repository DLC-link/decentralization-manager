import { test, expect } from "@playwright/test";
import { getAuthConfig, fetchRopcTokens, seedAuth } from "../fixtures/auth.js";

test("logs in headlessly as cvault-finoa-lp-1 on P1", async ({ browser }) => {
  const cfg = await getAuthConfig(8081);
  test.skip(!cfg.auth_required, "auth not required on this backend");
  const tokens = await fetchRopcTokens(cfg);
  const context = await browser.newContext();
  await seedAuth(context, tokens);
  const page = await context.newPage();
  await page.goto("http://localhost:8081/");
  // Auth success signal: the Sidebar nav renders only when authenticated
  // (otherwise AuthContext renders <LoginPage> instead of the app shell).
  // The Logout IconButton has no accessible name (icon-only + Tooltip), so we
  // assert on the "Parties" nav item instead.
  await expect(page.getByRole("button", { name: /Parties/ })).toBeVisible();
  await context.close();
});
