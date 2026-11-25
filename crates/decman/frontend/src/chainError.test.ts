import { describe, expect, it } from "vitest";

import { isChainError } from "./chainError";

// The texts the execute handler answers with.
describe("isChainError", () => {
  it("matches a ledger refusal", () => {
    expect(
      isChainError(
        "Failed to execute action: the ledger refused the submission (CONTRACT_NOT_FOUND). See the DecMan server log.",
      ),
    ).toBe(true);
  });

  it("matches a contract's own refusal with its message", () => {
    expect(
      isChainError(
        "Failed to execute action: the ledger refused the submission (DAML_FAILURE): the proposal needs one more confirmation. See the DecMan server log.",
      ),
    ).toBe(true);
  });

  it("matches a failed Ledger API call", () => {
    expect(
      isChainError(
        "Failed to execute action: the Ledger API answered Unavailable. See the DecMan server log.",
      ),
    ).toBe(true);
  });

  it("matches a failed Ledger API connection", () => {
    expect(
      isChainError(
        "Failed to execute action: the Ledger API connection failed. See the DecMan server log.",
      ),
    ).toBe(true);
  });

  it("leaves out a setting the operator can fix", () => {
    expect(
      isChainError(
        "Failed to execute action: DECPM_SCAN_URL is not set, so this node cannot read the open mining round. Set it to a Splice scan API to accept a paid credential.",
      ),
    ).toBe(false);
  });

  it("leaves out a scan refusal", () => {
    expect(
      isChainError(
        "Failed to execute action: Failed to fetch the open mining rounds from scan (scan answered 403 Forbidden). See the DecMan server log.",
      ),
    ).toBe(false);
  });
});
