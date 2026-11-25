import { describe, expect, it } from "vitest";

import { cantonIdName, sameNamespace, shortenId, splitCantonId } from "./cantonId";

const fp = "1220" + "0123456789abcdef".repeat(4);
const otherFp = "1220" + "fedcba9876543210".repeat(4);

describe("splitCantonId", () => {
  it("splits a party id into name and fingerprint", () => {
    expect(splitCantonId(`alice::${fp}`)).toEqual({ name: "alice", fingerprint: fp, suffix: "" });
  });

  it("keeps the trailing part of a physical synchronizer id", () => {
    expect(splitCantonId(`global-domain::${fp}::36-4`)).toEqual({
      name: "global-domain",
      fingerprint: fp,
      suffix: "::36-4",
    });
  });

  it("keeps a type prefix as part of the name", () => {
    expect(splitCantonId(`PAR::node-1::${fp}`)?.name).toBe("PAR::node-1");
  });

  it("rejects text without a hex fingerprint", () => {
    expect(splitCantonId("00ce819c551e")).toBeNull();
    expect(splitCantonId("alice::not-hex")).toBeNull();
  });
});

describe("shortenId", () => {
  it("keeps a long name whole and cuts only the fingerprint", () => {
    const id = `long-validator-party-name-1::${fp}`;
    expect(shortenId(id, 32, 16)).toBe(`long-validator-party-name-1::${fp.slice(0, 8)}...${fp.slice(-16)}`);
  });

  it("gives a short name's spare room to the fingerprint", () => {
    const id = `alice::${fp}`;
    // 32 - "alice::".length = 25 fingerprint characters.
    expect(shortenId(id, 32, 16)).toBe(`alice::${fp.slice(0, 25)}...${fp.slice(-16)}`);
  });

  it("keeps the synchronizer suffix after the shortened fingerprint", () => {
    expect(shortenId(`global-domain::${fp}::36-4`, 16, 8)).toBe(
      `global-domain::${fp.slice(0, 8)}...${fp.slice(-8)}::36-4`,
    );
  });

  it("leaves a fingerprint that already fits untouched", () => {
    expect(shortenId("alice::1220abcdef", 10, 8)).toBe("alice::1220abcdef");
  });

  it("cuts other text in the middle", () => {
    expect(shortenId("00ce819c551e0123456789d5a1d8f9", 12, 8)).toBe("00ce819c551e...d5a1d8f9");
  });

  it("leaves short text untouched", () => {
    expect(shortenId("00rules", 12, 8)).toBe("00rules");
  });
});

describe("cantonIdName", () => {
  it("returns the name of an id, or the whole text", () => {
    expect(cantonIdName(`validator-node-1::${fp}`)).toBe("validator-node-1");
    expect(cantonIdName("plain")).toBe("plain");
  });
});

describe("sameNamespace", () => {
  it("matches parties allocated under one participant's namespace", () => {
    expect(sameNamespace(`admin-party::${fp}`, `node-1::${fp}`)).toBe(true);
  });

  it("does not match different namespaces or ids without one", () => {
    expect(sameNamespace(`alice::${fp}`, `bob::${otherFp}`)).toBe(false);
    expect(sameNamespace("plain", "plain")).toBe(false);
  });
});
