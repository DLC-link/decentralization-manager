import { describe, expect, it } from "vitest";

import { kickThresholdBound } from "./kickThreshold";

describe("kickThresholdBound", () => {
  // Three hosts and four owners: one owner hosts nothing.
  it("leaves a host's kick the other hosts' keys to sign with", () => {
    expect(kickThresholdBound(true, 3, 4)).toBe(2);
  });

  it("leaves an owner-only kick every host", () => {
    expect(kickThresholdBound(false, 3, 4)).toBe(3);
  });

  it("bounds by the hosts when owners outnumber them", () => {
    expect(kickThresholdBound(false, 2, 4)).toBe(2);
  });

  it("bounds by the hosts alone when no owner is cached", () => {
    expect(kickThresholdBound(true, 4, 0)).toBe(3);
  });
});
