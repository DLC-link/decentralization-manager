import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./tests",
  fullyParallel: false,
  workers: 1,
  retries: 0,
  timeout: 5 * 60_000,
  expect: { timeout: 30_000 },
  globalSetup: "./global-setup.ts",
  globalTeardown: "./global-teardown.ts",
  reporter: [["html", { open: "never" }], ["list"]],
  use: {
    // Capture on failure only; the phases attach their own intentional
    // full-page screenshots via test.info().attach() regardless of this.
    screenshot: "only-on-failure",
    trace: "retain-on-failure",
    actionTimeout: 30_000,
    viewport: { width: 1440, height: 900 }, // desktop: render the Sidebar nav, not the mobile Header
  },
});
