import { describe, expect, it } from "vitest";

import { buildHash, parseHash, sharedWithSlug } from "./hashRoute";

const ns = "a".repeat(68);
const participant = `bravo::${ns}`;

describe("parseHash", () => {
  it("reads the shared view's participant and no party slug", () => {
    const route = parseHash(`#parties/shared/${participant}`);

    expect(route.tab).toBe(0);
    expect(route.sharedWith).toBe(participant);
    expect(route.partySlug).toBeNull();
  });

  it("reads the peer a config link points at", () => {
    const route = parseHash(`#config/${participant}`);

    expect(route.tab).toBe(2);
    expect(route.configPeerId).toBe(participant);
    expect(route.partySlug).toBeNull();
    expect(route.sharedWith).toBeNull();
  });

  it("keeps a party id and a prefix as party slugs", () => {
    expect(parseHash(`#parties/alpha::${ns}`)).toMatchObject({
      tab: 0,
      partySlug: `alpha::${ns}`,
      sharedWith: null,
    });
    expect(parseHash("#parties/cbtc-network")).toMatchObject({
      partySlug: "cbtc-network",
      sharedWith: null,
    });
  });

  it("does not take a bare `shared` prefix for the shared view", () => {
    expect(parseHash("#parties/shared")).toMatchObject({
      partySlug: "shared",
      sharedWith: null,
    });
  });

  it("ignores a shared slug with no participant", () => {
    expect(parseHash("#parties/shared/")).toMatchObject({
      partySlug: null,
      sharedWith: null,
    });
  });

  it("reads no peer from a bare config hash, and no shared view off the parties tab", () => {
    expect(parseHash("#config").configPeerId).toBeNull();
    expect(parseHash(`#packages/shared/${participant}`).sharedWith).toBeNull();
  });
});

describe("buildHash", () => {
  it("round-trips the shared view and a config link through parseHash", () => {
    const shared = buildHash(0, sharedWithSlug(participant));
    expect(shared).toBe(`#parties/shared/${participant}`);
    expect(parseHash(shared).sharedWith).toBe(participant);

    const config = buildHash(2, participant);
    expect(config).toBe(`#config/${participant}`);
    expect(parseHash(config).configPeerId).toBe(participant);
  });
});
