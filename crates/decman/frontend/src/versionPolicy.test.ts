import { describe, expect, it } from "vitest";

import cargoToml from "../../Cargo.toml?raw";
import policyFile from "../../../../version-policy.json?raw";
import { parsePolicy, upgradeNotice } from "./versionPolicy";

const policy = (latest: string, minimum_supported: string) => ({ latest, minimum_supported });

describe("upgradeNotice", () => {
  it("shows nothing on the latest release", () => {
    expect(upgradeNotice("1.17.1", policy("1.17.1", "1.14.0"))).toBeNull();
  });

  it("shows nothing on a build newer than the latest release", () => {
    expect(upgradeNotice("1.18.0", policy("1.17.1", "1.14.0"))).toBeNull();
  });

  it("offers an update to a supported node behind the latest release", () => {
    expect(upgradeNotice("1.16.0", policy("1.17.1", "1.14.0"))).toEqual({
      kind: "update",
      current: "1.16.0",
      latest: "1.17.1",
    });
  });

  it("treats the minimum supported version itself as supported", () => {
    expect(upgradeNotice("1.14.0", policy("1.17.1", "1.14.0"))?.kind).toBe("update");
  });

  it("marks a node below the minimum supported version as critical", () => {
    expect(upgradeNotice("1.13.9", policy("1.17.1", "1.14.0"))).toEqual({
      kind: "critical",
      current: "1.13.9",
      latest: "1.17.1",
      minimum: "1.14.0",
    });
  });

  it("compares each part as a number", () => {
    expect(upgradeNotice("1.9.0", policy("1.10.0", "1.10.0"))?.kind).toBe("critical");
    expect(upgradeNotice("1.10.0", policy("1.9.0", "1.9.0"))).toBeNull();
  });

  it("shows nothing for a dev build", () => {
    expect(upgradeNotice("dev", policy("1.17.1", "1.14.0"))).toBeNull();
  });
});

describe("parsePolicy", () => {
  it("normalizes a v prefix away", () => {
    expect(parsePolicy({ latest: "v1.17.1", minimum_supported: "v1.14.0" })).toEqual(
      policy("1.17.1", "1.14.0"),
    );
  });

  it.each([
    ["no object", null],
    ["a missing field", { latest: "1.17.1" }],
    ["a non-string field", { latest: 1.17, minimum_supported: "1.14.0" }],
    ["a pre-release tag", { latest: "1.18.0-rc.1", minimum_supported: "1.14.0" }],
    ["a minimum above latest", { latest: "1.14.0", minimum_supported: "1.17.1" }],
  ])("rejects %s", (_label, raw) => {
    expect(parsePolicy(raw)).toBeNull();
  });
});

describe("version-policy.json", () => {
  it("lists the crate version as the latest release", () => {
    const crateVersion = /^version\s*=\s*"([^"]+)"/m.exec(cargoToml)?.[1];
    const parsed = parsePolicy(JSON.parse(policyFile));

    expect(parsed, "version-policy.json must be valid, with minimum_supported <= latest").not.toBeNull();
    expect(
      parsed?.latest,
      "A version bump must set `latest` in version-policy.json to the crates/decman/Cargo.toml version",
    ).toBe(crateVersion);
  });
});
