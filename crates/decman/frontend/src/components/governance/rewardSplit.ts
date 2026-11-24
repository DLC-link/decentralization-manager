// Daml's Decimal scale for a reward-split percentage: 10 places.
const SPLIT_SCALE = 10_000_000_000n;

/** Render a scaled integer as the fixed-point decimal the ledger stores. */
const formatSplitShare = (scaled: bigint): string => {
  const s = scaled.toString().padStart(11, "0");
  return `${s.slice(0, -10)}.${s.slice(-10)}`;
};

/**
 * Turn integer weights into shares that sum to EXACTLY 1.0.
 *
 * The ledger compares the sum as exact Decimal, with no tolerance, so an even
 * 3-way split is not expressible as a repeated decimal: 0.3333333333 three
 * times is 0.9999999999 and the vote fails at execute, after the
 * confirmations are already spent. Rather than ask a human to hand-balance
 * the last entry, take integer weights and derive the decimals.
 *
 * Floor each `weight / total`, then give every leftover unit to the LARGEST
 * weight (ties by row order). The rule is deterministic on purpose: a
 * confirmer has to be able to reproduce the split from the weights alone, so
 * "whichever row happened to be picked" is not good enough. Distortion is at
 * most (n-1) × 1e-10.
 *
 * Returns null when the weights cannot produce a valid split.
 */
const splitFromWeights = (weights: bigint[]): bigint[] | null => {
  const total = weights.reduce((a, w) => a + w, 0n);
  if (total <= 0n) return null;
  const shares = weights.map((w) => (w * SPLIT_SCALE) / total);
  const leftover = SPLIT_SCALE - shares.reduce((a, s) => a + s, 0n);
  if (leftover > 0n) {
    let largest = 0;
    weights.forEach((w, i) => {
      if (w > weights[largest]) largest = i;
    });
    shares[largest] += leftover;
  }
  return shares;
};

/** Index of the row that absorbs the rounding remainder, for the UI to mark. */
const splitRemainderRow = (weights: bigint[]): number => {
  let largest = 0;
  weights.forEach((w, i) => {
    if (w > weights[largest]) largest = i;
  });
  return largest;
};

/** Parse the weight column; null if any entry is not a positive integer. */
const parseSplitWeights = (
  split: { beneficiary: string; weight: string }[],
): bigint[] | null => {
  const out: bigint[] = [];
  for (const row of split) {
    const w = row.weight.trim();
    if (!/^\d+$/.test(w)) return null;
    const v = BigInt(w);
    if (v <= 0n) return null;
    out.push(v);
  }
  return out;
};

const validateDelegationSplit = (
  split: { beneficiary: string; weight: string }[],
): string | null => {
  if (split.length === 0) return "Add at least one beneficiary";
  if (split.length > 20) return "At most 20 beneficiaries";
  if (split.some((b) => !b.beneficiary.trim())) {
    return "Every row needs a beneficiary party";
  }
  const parties = split.map((b) => b.beneficiary.trim());
  if (new Set(parties).size !== parties.length) {
    return "Each beneficiary may appear only once";
  }
  const weights = parseSplitWeights(split);
  if (!weights) return "Each weight must be a whole number greater than 0";
  const shares = splitFromWeights(weights);
  if (!shares) return "Weights must add up to more than 0";
  // Daml requires every percentage in (0, 1]. A weight tiny enough against the
  // total floors to zero, which the ledger rejects — catch it here instead.
  const zeroAt = shares.findIndex((s) => s <= 0n);
  if (zeroAt >= 0) {
    return `Row ${zeroAt + 1}'s weight is too small against the total — its share rounds to 0, which the ledger rejects`;
  }
  return null;
};

export {
  SPLIT_SCALE,
  formatSplitShare,
  parseSplitWeights,
  splitFromWeights,
  splitRemainderRow,
  validateDelegationSplit,
};
