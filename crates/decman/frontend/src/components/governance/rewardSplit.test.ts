import { describe, expect, it } from "vitest";

import {
  SPLIT_SCALE,
  formatSplitShare,
  parseSplitWeights,
  splitFromWeights,
  splitRemainderRow,
  validateDelegationSplit,
} from "./rewardSplit";

const rows = (...weights: string[]) =>
  weights.map((weight, i) => ({ beneficiary: `p${i}::1`, weight }));

describe("formatSplitShare", () => {
  it("writes ten decimal places", () => {
    expect(formatSplitShare(SPLIT_SCALE)).toBe("1.0000000000");
    expect(formatSplitShare(3_333_333_334n)).toBe("0.3333333334");
    expect(formatSplitShare(5n)).toBe("0.0000000005");
  });
});

describe("parseSplitWeights", () => {
  it("reads whole numbers and trims them", () => {
    expect(parseSplitWeights(rows(" 80 ", "20"))).toEqual([80n, 20n]);
  });

  it("refuses zero, decimals, signs and text", () => {
    for (const bad of ["0", "1.5", "-1", "+1", "x", ""]) {
      expect(parseSplitWeights(rows("1", bad))).toBeNull();
    }
  });
});

describe("splitFromWeights", () => {
  it("gives the rounding remainder to the largest weight", () => {
    // Floors: 0.2857142857, 0.2857142857, 0.4285714285; one unit is left.
    const shares = splitFromWeights([2n, 2n, 3n])!;
    expect(shares.map(formatSplitShare)).toEqual([
      "0.2857142857",
      "0.2857142857",
      "0.4285714286",
    ]);
    expect(shares.reduce((a, s) => a + s, 0n)).toBe(SPLIT_SCALE);
    expect(splitRemainderRow([2n, 2n, 3n])).toBe(2);
  });

  it("gives an even three-way remainder to the first row", () => {
    const shares = splitFromWeights([1n, 1n, 1n])!;
    expect(shares.map(formatSplitShare)).toEqual([
      "0.3333333334",
      "0.3333333333",
      "0.3333333333",
    ]);
    expect(splitRemainderRow([1n, 1n, 1n])).toBe(0);
  });

  it("returns null when the weights add up to nothing", () => {
    expect(splitFromWeights([])).toBeNull();
    expect(splitFromWeights([0n])).toBeNull();
  });
});

describe("validateDelegationSplit", () => {
  it("accepts a valid split", () => {
    expect(validateDelegationSplit(rows("80", "20"))).toBeNull();
  });

  it("names each problem in the order it checks them", () => {
    expect(validateDelegationSplit([])).toBe("Add at least one beneficiary");
    expect(validateDelegationSplit(rows(...Array(21).fill("1")))).toBe("At most 20 beneficiaries");
    expect(validateDelegationSplit([{ beneficiary: " ", weight: "1" }])).toBe(
      "Every row needs a beneficiary party",
    );
    expect(
      validateDelegationSplit([
        { beneficiary: "a::1", weight: "1" },
        { beneficiary: " a::1 ", weight: "1" },
      ]),
    ).toBe("Each beneficiary may appear only once");
    expect(validateDelegationSplit(rows("1", "0"))).toBe(
      "Each weight must be a whole number greater than 0",
    );
  });

  it("refuses a weight whose share rounds to zero", () => {
    expect(validateDelegationSplit(rows("100000000000", "1"))).toBe(
      "Row 2's weight is too small against the total — its share rounds to 0, which the ledger rejects",
    );
  });
});
