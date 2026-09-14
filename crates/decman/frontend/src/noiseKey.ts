// Browser-side equivalent of `secp256k1::PublicKey::from_slice`, which every
// Noise path runs on a peer's key. Hex and length are not enough: `02` and 64
// zeroes is neither on the curve nor usable, and a peer carrying one silently
// never connects.

/** Field prime of secp256k1: 2^256 - 2^32 - 977. */
const P = (1n << 256n) - (1n << 32n) - 977n;

const modPow = (base: bigint, exponent: bigint, modulus: bigint): bigint => {
  let result = 1n;
  let b = base % modulus;
  let e = exponent;
  while (e > 0n) {
    if (e & 1n) result = (result * b) % modulus;
    b = (b * b) % modulus;
    e >>= 1n;
  }
  return result;
};

const inField = (v: bigint): boolean => v > 0n && v < P;

/** y² = x³ + 7 (mod p). */
const satisfiesCurve = (x: bigint, y: bigint): boolean =>
  (((y * y - (x * x * x + 7n)) % P) + P) % P === 0n;

/** Whether `hex` is a public key the backend's secp256k1 parser would accept. */
export const isNoisePublicKey = (hex: string): boolean => {
  if (!/^[0-9a-fA-F]+$/.test(hex)) return false;
  const prefix = hex.slice(0, 2).toLowerCase();

  if (hex.length === 66 && (prefix === "02" || prefix === "03")) {
    const x = BigInt(`0x${hex.slice(2)}`);
    if (!inField(x)) return false;
    // p ≡ 3 (mod 4), so where a square root exists it is v^((p+1)/4).
    const alpha = (modPow(x, 3n, P) + 7n) % P;
    const y = modPow(alpha, (P + 1n) / 4n, P);
    return y !== 0n && (y * y) % P === alpha;
  }

  if (hex.length === 130 && prefix === "04") {
    const x = BigInt(`0x${hex.slice(2, 66)}`);
    const y = BigInt(`0x${hex.slice(66)}`);
    return inField(x) && inField(y) && satisfiesCurve(x, y);
  }

  return false;
};
