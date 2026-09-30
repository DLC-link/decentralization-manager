export interface BeneficiaryRow {
  beneficiary: string;
  weight: string;
}

// Daml Decimal: up to 10 fractional digits.
const SCALE = 10;
const ONE = 10n ** BigInt(SCALE);

/** A weight as an exact scaled integer, or null when it is not a decimal. */
const parseWeight = (text: string): bigint | null => {
  const match = /^(\d+)(?:\.(\d{1,10}))?$/.exec(text.trim());
  if (!match) return null;
  return BigInt(match[1]) * ONE + BigInt((match[2] ?? "").padEnd(SCALE, "0"));
};

const formatScaled = (value: bigint): string => {
  const whole = value / ONE;
  const fraction = (value % ONE).toString().padStart(SCALE, "0").replace(/0+$/, "");
  return fraction ? `${whole}.${fraction}` : `${whole}`;
};

/**
 * Exact sum of the row weights, e.g. "0.7", or null while a weight is not a
 * valid decimal. Exact because the template compares against 1.0 exactly:
 * 0.3 + 0.6 + 0.1 must count as 1.0, which a float sum does not.
 */
export const beneficiaryWeightSum = (rows: BeneficiaryRow[]): string | null => {
  let sum = 0n;
  for (const row of rows) {
    const weight = parseWeight(row.weight);
    if (weight === null) return null;
    sum += weight;
  }
  return formatScaled(sum);
};

/**
 * Why a Set Provider App Reward Beneficiaries proposal cannot be submitted,
 * or null when it can. The InstrumentConfiguration template rejects an empty
 * list and weights that do not sum to exactly 1.0, and a proposal stores its
 * payload, so one the template rejects can never execute: it has to be
 * retracted and proposed again after the committee has voted.
 */
export const beneficiariesProblem = (
  rows: BeneficiaryRow[],
  clear: boolean,
): string | null => {
  if (clear) return null;
  if (rows.length === 0) {
    return "Add at least one beneficiary, or tick Clear beneficiaries";
  }
  for (const [index, row] of rows.entries()) {
    if (!row.beneficiary.trim() || !row.weight.trim()) {
      return `Beneficiary row ${index + 1}: party and weight are required`;
    }
    if (parseWeight(row.weight) === null) {
      return `Beneficiary row ${index + 1}: weight must be a decimal such as 0.25`;
    }
  }
  const sum = beneficiaryWeightSum(rows);
  if (sum !== "1") return `Weights must sum to 1.0 (now ${sum})`;
  return null;
};
