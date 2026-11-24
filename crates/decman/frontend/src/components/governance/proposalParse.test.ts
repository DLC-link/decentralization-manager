import { describe, expect, it } from "vitest";

import {
  buildRequirements,
  credentialValidity,
  parseClaimsText,
  parsePartyLines,
  parseRequiredClaimsText,
  requiredClaimsText,
} from "./proposalParse";

describe("parseClaimsText", () => {
  it("reads one claim per line and trims every part", () => {
    expect(parseClaimsText(" alice::1 , kyc , true \n\nbob::2,role,issuer\n")).toEqual([
      { subject: "alice::1", property: "kyc", value: "true" },
      { subject: "bob::2", property: "role", value: "issuer" },
    ]);
  });

  it("returns no claims for blank text", () => {
    expect(parseClaimsText(" \n \n")).toEqual([]);
  });

  it("names the bad line when a part is missing", () => {
    expect(() => parseClaimsText("alice::1,kyc,true\nbob::2,role")).toThrow(
      'Claim line 2: expected "<subject>,<property>,<value>", got "bob::2,role"',
    );
  });

  it("refuses an empty part and a fourth part", () => {
    expect(() => parseClaimsText("alice::1,,true")).toThrow("Claim line 1");
    expect(() => parseClaimsText("alice::1,kyc,true,extra")).toThrow("Claim line 1");
  });
});

describe("parseRequiredClaimsText", () => {
  it("reads property,value lines", () => {
    expect(parseRequiredClaimsText("kyc, true\n role ,issuer")).toEqual([
      { property: "kyc", value: "true" },
      { property: "role", value: "issuer" },
    ]);
  });

  it("refuses a line with a subject", () => {
    expect(() => parseRequiredClaimsText("alice::1,kyc,true")).toThrow(
      'Claim line 1: expected "<property>,<value>", got "alice::1,kyc,true"',
    );
  });
});

describe("requiredClaimsText", () => {
  it("writes the editor text that parseRequiredClaimsText reads back", () => {
    const claims = [
      { property: "kyc", value: "true" },
      { property: "role", value: "issuer" },
    ];
    expect(requiredClaimsText(claims)).toBe("kyc,true\nrole,issuer");
    expect(parseRequiredClaimsText(requiredClaimsText(claims))).toEqual(claims);
  });
});

describe("buildRequirements", () => {
  it("trims the issuer and parses the claims text", () => {
    expect(
      buildRequirements([{ issuer: " reg::1 ", claimsText: "kyc,true" }], "Issuer"),
    ).toEqual([{ issuer: "reg::1", required_claims: [{ property: "kyc", value: "true" }] }]);
  });

  it("sends a prefilled row's exact claims while its text is unchanged", () => {
    // A value with a comma cannot survive the text form.
    const claims = [{ property: "name", value: "a, b" }];
    const row = { issuer: "reg::1", claimsText: requiredClaimsText(claims), claims };
    expect(buildRequirements([row], "Holder")).toEqual([
      { issuer: "reg::1", required_claims: claims },
    ]);
  });

  it("parses the text once a prefilled row is edited", () => {
    const row = {
      issuer: "reg::1",
      claimsText: "kyc,false",
      claims: [{ property: "kyc", value: "true" }],
    };
    expect(buildRequirements([row], "Holder")[0].required_claims).toEqual([
      { property: "kyc", value: "false" },
    ]);
  });

  it("names the list and the row when the issuer is empty", () => {
    expect(() =>
      buildRequirements(
        [
          { issuer: "reg::1", claimsText: "" },
          { issuer: "  ", claimsText: "" },
        ],
        "Registrar",
      ),
    ).toThrow("Registrar requirement 2: issuer party is required");
  });
});

describe("parsePartyLines", () => {
  it("keeps the order of the trimmed, non-empty lines", () => {
    expect(parsePartyLines(" b::1 \n\na::1\n")).toEqual(["b::1", "a::1"]);
  });

  it("refuses a party listed twice", () => {
    expect(() => parsePartyLines("a::1\nb::1\n a::1")).toThrow(
      "Party id listed more than once: a::1",
    );
  });
});

describe("credentialValidity", () => {
  it("leaves both sides unbounded when the inputs are empty", () => {
    expect(credentialValidity("", "")).toEqual({
      credential_valid_from_micros: undefined,
      credential_valid_until_micros: undefined,
    });
  });

  it("converts each side to micros since epoch", () => {
    const from = "2026-01-01T00:00";
    const until = "2026-02-01T00:00";
    expect(credentialValidity(from, until)).toEqual({
      credential_valid_from_micros: new Date(from).getTime() * 1000,
      credential_valid_until_micros: new Date(until).getTime() * 1000,
    });
  });

  it("refuses a window that does not end after it starts", () => {
    expect(() => credentialValidity("2026-02-01T00:00", "2026-02-01T00:00")).toThrow(
      "Credentials Valid From must be before Credentials Valid Until",
    );
  });

  it("names the side that is not a date", () => {
    expect(() => credentialValidity("", "not a date")).toThrow(
      "Credentials Valid Until must be a valid date and time",
    );
  });

  it("refuses a date whose micros lose precision", () => {
    expect(() => credentialValidity("9999-01-01T00:00", "")).toThrow(
      "Credentials Valid From is too far in the future",
    );
  });
});
