import { execFileSync } from "node:child_process";
import path from "node:path";

const REPO_ROOT = path.resolve(import.meta.dirname, "..");

export default async function globalTeardown() {
  if (process.env.E2E_ASSUME_STACK_UP === "1") return;
  execFileSync(
    path.join(REPO_ROOT, "integration-tests", "bring-up.sh"),
    ["--teardown"],
    { stdio: "inherit" },
  );
}
