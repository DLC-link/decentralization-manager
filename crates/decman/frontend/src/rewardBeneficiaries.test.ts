import { describe, expect, it } from "vitest";

import { beneficiariesProblem, beneficiaryWeightSum } from "./rewardBeneficiaries";

const row = (beneficiary: string, weight: string) => ({ beneficiary, weight });

describe("beneficiariesProblem", () => {
  it("refuses an empty list unless the clear toggle is on", () => {
    expect(beneficiariesProblem([], false)).toBe(
      "Add at least one beneficiary, or tick Clear beneficiaries",
    );
    expect(beneficiariesProblem([], true)).toBeNull();
  });

  it("ignores the rows when clearing", () => {
    expect(beneficiariesProblem([row("", "")], true)).toBeNull();
  });

  it("requires a party and a decimal weight on every row", () => {
    expect(beneficiariesProblem([row("", "1")], false)).toBe(
      "Beneficiary row 1: party and weight are required",
    );
    expect(beneficiariesProblem([row("alice::1220", "half")], false)).toBe(
      "Beneficiary row 1: weight must be a decimal such as 0.25",
    );
    expect(beneficiariesProblem([row("alice::1220", "-1")], false)).toBe(
      "Beneficiary row 1: weight must be a decimal such as 0.25",
    );
  });

  it("requires the weights to sum to exactly 1.0", () => {
    expect(beneficiariesProblem([row("alice::1220", "0.7")], false)).toBe(
      "Weights must sum to 1.0 (now 0.7)",
    );
    expect(
      beneficiariesProblem([row("alice::1220", "0.6"), row("bob::1220", "0.6")], false),
    ).toBe("Weights must sum to 1.0 (now 1.2)");
    expect(beneficiariesProblem([row("alice::1220", "1")], false)).toBeNull();
  });

  it("accepts a split a float sum gets wrong", () => {
    const rows = [row("a::1220", "0.3"), row("b::1220", "0.6"), row("c::1220", "0.1")];
    expect(0.3 + 0.6 + 0.1).not.toBe(1);
    expect(beneficiariesProblem(rows, false)).toBeNull();
  });

  it("compares the sum exactly, without a tolerance", () => {
    // Within 1e-9 of 1.0, so a tolerant float check accepted it; the template
    // compares Decimals exactly and rejects it.
    expect(
      beneficiariesProblem([row("a::1220", "0.5"), row("b::1220", "0.5000000001")], false),
    ).toBe("Weights must sum to 1.0 (now 1.0000000001)");
  });

  it("requires every weight to be greater than 0 and at most 1", () => {
    expect(beneficiariesProblem([row("a::1220", "1"), row("b::1220", "0")], false)).toBe(
      "Beneficiary row 2: weight must be greater than 0 and at most 1",
    );
    expect(beneficiariesProblem([row("a::1220", "0.000")], false)).toBe(
      "Beneficiary row 1: weight must be greater than 0 and at most 1",
    );
    expect(beneficiariesProblem([row("a::1220", "1.5")], false)).toBe(
      "Beneficiary row 1: weight must be greater than 0 and at most 1",
    );
  });

  it("refuses the same party twice, ignoring surrounding spaces", () => {
    expect(
      beneficiariesProblem([row("a::1220", "0.5"), row(" a::1220 ", "0.5")], false),
    ).toBe("Beneficiary row 2: a::1220 is already listed");
  });

  it("allows at most 19 beneficiaries", () => {
    const split = (n: number, weight: string) =>
      Array.from({ length: n }, (_, i) => row(`p${i}::1220`, weight));
    const nineteen = [...split(18, "0.05"), row("last::1220", "0.1")];
    expect(beneficiariesProblem(nineteen, false)).toBeNull();
    expect(beneficiariesProblem(split(20, "0.05"), false)).toBe(
      "At most 19 beneficiaries (now 20)",
    );
  });
});

describe("beneficiaryWeightSum", () => {
  it("adds exactly and drops trailing zeros", () => {
    expect(beneficiaryWeightSum([row("a", "0.25"), row("b", "0.50")])).toBe("0.75");
    expect(beneficiaryWeightSum([])).toBe("0");
  });

  it("is null while a weight is not a decimal", () => {
    expect(beneficiaryWeightSum([row("a", "0.5"), row("b", "")])).toBeNull();
  });
});
