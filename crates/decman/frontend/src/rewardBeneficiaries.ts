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

/** The template needs fewer than 20 beneficiaries. */
export const MAX_BENEFICIARIES = 19;

/**
 * Why a Set Provider App Reward Beneficiaries proposal cannot be submitted,
 * or null when it can. These are the InstrumentConfiguration template's
 * rules (`areValidProviderAppRewardBeneficiaries`): at least one and at most
 * 19 beneficiaries, each weight greater than 0 and at most 1, no party
 * twice, and weights summing to exactly 1.0. A proposal stores its payload,
 * so one the template rejects can never execute: it has to be retracted and
 * proposed again after the committee has voted.
 */
export const beneficiariesProblem = (
  rows: BeneficiaryRow[],
  clear: boolean,
): string | null => {
  if (clear) return null;
  if (rows.length === 0) {
    return "Add at least one beneficiary, or tick Clear beneficiaries";
  }
  if (rows.length > MAX_BENEFICIARIES) {
    return `At most ${MAX_BENEFICIARIES} beneficiaries (now ${rows.length})`;
  }
  const seen = new Set<string>();
  for (const [index, row] of rows.entries()) {
    const party = row.beneficiary.trim();
    if (!party || !row.weight.trim()) {
      return `Beneficiary row ${index + 1}: party and weight are required`;
    }
    const weight = parseWeight(row.weight);
    if (weight === null) {
      return `Beneficiary row ${index + 1}: weight must be a decimal such as 0.25`;
    }
    if (weight === 0n || weight > ONE) {
      return `Beneficiary row ${index + 1}: weight must be greater than 0 and at most 1`;
    }
    if (seen.has(party)) {
      return `Beneficiary row ${index + 1}: ${party} is already listed`;
    }
    seen.add(party);
  }
  const sum = beneficiaryWeightSum(rows);
  if (sum !== "1") return `Weights must sum to 1.0 (now ${sum})`;
  return null;
};
