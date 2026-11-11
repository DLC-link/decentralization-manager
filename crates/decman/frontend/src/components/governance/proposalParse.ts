import type { Claim, PartyCredentialRequirement } from "../../types";
import type { RequirementRow } from "./fields/RequirementRows";

/** The editor's "property,value" text for a list of required claims. */
const requiredClaimsText = (claims: { property: string; value: string }[]) =>
  claims.map((claim) => `${claim.property},${claim.value}`).join("\n");

// Parse a multi-line "subject,property,value" textarea into a Claim[].
// Mirrors the comma-split-with-error-on-bad-line pattern used by
// set_provider_app_reward_beneficiaries.
const parseClaimsText = (text: string): Claim[] => {
  const lines = text
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean);
  return lines.map((line, idx) => {
    const parts = line.split(",").map((s) => s.trim());
    if (parts.length !== 3 || !parts[0] || !parts[1] || !parts[2]) {
      throw new Error(
        `Claim line ${idx + 1}: expected "<subject>,<property>,<value>", got "${line}"`,
      );
    }
    return { subject: parts[0], property: parts[1], value: parts[2] };
  });
};

// Parse one requirement row's claims textarea. Each line is
// "<property>,<value>" — parseClaimsText minus the subject, because a
// requirement applies to whichever party gets credentialed.
const parseRequiredClaimsText = (
  text: string,
): { property: string; value: string }[] => {
  const lines = text
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean);
  return lines.map((line, idx) => {
    const parts = line.split(",").map((s) => s.trim());
    if (parts.length !== 2 || !parts[0] || !parts[1]) {
      throw new Error(
        `Claim line ${idx + 1}: expected "<property>,<value>", got "${line}"`,
      );
    }
    return { property: parts[0], value: parts[1] };
  });
};

// Turn requirement-editor rows into PartyCredentialRequirement payloads.
// `label` names the list in error messages. A prefilled row whose text is
// unchanged sends its original claims, because the text form cannot carry
// a comma, a newline or edge whitespace inside a value.
const buildRequirements = (
  rows: RequirementRow[],
  label: string,
): PartyCredentialRequirement[] =>
  rows.map((row, idx) => {
    const issuer = row.issuer.trim();
    if (!issuer) {
      throw new Error(`${label} requirement ${idx + 1}: issuer party is required`);
    }
    return {
      issuer,
      required_claims:
        row.claims && row.claimsText === requiredClaimsText(row.claims)
          ? row.claims
          : parseRequiredClaimsText(row.claimsText),
    };
  });

// Split a one-party-per-line textarea into party ids. A duplicated party
// is an error, not silently deduplicated: the templates refuse duplicates
// (two mints for one subject would share a credential id), and dropping a
// line here would hide what the operator actually typed.
const parsePartyLines = (text: string): string[] => {
  const lines = text
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean);
  const seen = new Set<string>();
  for (const line of lines) {
    if (seen.has(line)) {
      throw new Error(`Party id listed more than once: ${line}`);
    }
    seen.add(line);
  }
  return lines;
};

// The credential validity window of the two instrument proposals, in
// micros since epoch. An empty input leaves that side unbounded.
// Onboard Registrar has none: its credential is checked only once.
const credentialValidity = (validFrom: string, validUntil: string) => {
  const toMicros = (value: string, label: string) => {
    if (!value) return undefined;
    const ms = new Date(value).getTime();
    if (!Number.isFinite(ms)) {
      throw new Error(`${label} must be a valid date and time`);
    }
    if (!Number.isSafeInteger(ms * 1000)) {
      throw new Error(`${label} is too far in the future`);
    }
    return ms * 1000;
  };
  const from = toMicros(validFrom, "Credentials Valid From");
  const until = toMicros(validUntil, "Credentials Valid Until");
  if (from !== undefined && until !== undefined && from >= until) {
    throw new Error("Credentials Valid From must be before Credentials Valid Until");
  }
  return {
    credential_valid_from_micros: from,
    credential_valid_until_micros: until,
  };
};

export {
  buildRequirements,
  credentialValidity,
  parseClaimsText,
  parsePartyLines,
  parseRequiredClaimsText,
  requiredClaimsText,
};
