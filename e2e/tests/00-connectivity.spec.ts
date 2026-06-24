import { test, expect } from "@playwright/test";

for (const [name, port] of [["P1", 8081], ["P2", 8082], ["P3", 8083]] as const) {
  test(`${name} serves the SPA`, async ({ page }) => {
    const res = await page.goto(`http://localhost:${port}/`);
    expect(res?.status()).toBe(200);
    await expect(page.locator("#root")).toBeAttached();
  });
}
