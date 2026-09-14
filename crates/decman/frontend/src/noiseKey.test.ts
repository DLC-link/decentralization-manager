import { describe, expect, it } from "vitest";

import { isNoisePublicKey } from "./noiseKey";

// The generator point, the one secp256k1 value that can be written down.
const G_X = "79be667ef9dcbbac55a06295ce870b07029bfcdb2dce28d959f2815b16f81798";
const G_Y = "483ada7726a3c4655da4fbfc0e1108a8fd17b448a68554199c47d08ffb10d4b8";

describe("isNoisePublicKey", () => {
  it("accepts the generator in every encoding the backend takes", () => {
    expect(isNoisePublicKey(`02${G_X}`)).toBe(true);
    expect(isNoisePublicKey(`03${G_X}`)).toBe(true);
    expect(isNoisePublicKey(`04${G_X}${G_Y}`)).toBe(true);
    expect(isNoisePublicKey(`02${G_X}`.toUpperCase())).toBe(true);
  });

  // The point of this module: these all pass a hex-and-length check.
  it("rejects well-formed hex that is not a point on the curve", () => {
    expect(isNoisePublicKey(`02${"0".repeat(64)}`)).toBe(false);
    expect(isNoisePublicKey(`02${"f".repeat(64)}`)).toBe(false);
    expect(isNoisePublicKey(`04${G_X}${"0".repeat(64)}`)).toBe(false);
    expect(isNoisePublicKey(`04${G_Y}${G_X}`)).toBe(false);
  });

  it("rejects the wrong prefix, length or alphabet", () => {
    expect(isNoisePublicKey(`05${G_X}`)).toBe(false);
    expect(isNoisePublicKey(`02${G_X}${G_Y}`)).toBe(false);
    expect(isNoisePublicKey(`04${G_X}`)).toBe(false);
    expect(isNoisePublicKey(`02${G_X.slice(0, 62)}zz`)).toBe(false);
    expect(isNoisePublicKey("")).toBe(false);
  });

  // Compressed keys carry only x, so both parities of an on-curve x are valid
  // and exactly half of all x values have a square root at all.
  it("accepts either parity of an on-curve x", () => {
    const x = G_X;
    expect(isNoisePublicKey(`02${x}`)).toBe(isNoisePublicKey(`03${x}`));
  });
});
