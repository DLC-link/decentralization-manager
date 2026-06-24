import { execFileSync } from "node:child_process";
import { setTimeout as sleep } from "node:timers/promises";
import path from "node:path";

const REPO_ROOT = path.resolve(import.meta.dirname, "..");
const PORTS = [8081, 8082, 8083];

async function waitForHttp(port: number, deadlineMs: number) {
  const end = Date.now() + deadlineMs;
  while (Date.now() < end) {
    try {
      const res = await fetch(`http://localhost:${port}/`);
      if (res.ok) return;
    } catch { /* not up yet */ }
    await sleep(2000);
  }
  throw new Error(`port ${port} did not serve HTTP within ${deadlineMs}ms`);
}

export default async function globalSetup() {
  if (process.env.E2E_ASSUME_STACK_UP === "1") return; // skip bring-up if already running
  execFileSync(path.join(REPO_ROOT, "integration-tests", "bring-up.sh"), {
    stdio: "inherit",
    timeout: 15 * 60_000, // cargo build can be slow on a cold cache
  });
  for (const p of PORTS) await waitForHttp(p, 120_000);
}
