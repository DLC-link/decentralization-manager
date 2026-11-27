/**
 * Every node's UI reads the release policy from `version-policy.json` on
 * `main`. The browser fetches it, so the node itself needs no internet access.
 * A test keeps `latest` equal to the crate version, so a version bump that
 * forgets the file fails CI.
 */
export const VERSION_POLICY_URL =
  "https://raw.githubusercontent.com/DLC-link/decentralization-manager/main/version-policy.json";

const RELEASES_URL = "https://github.com/DLC-link/decentralization-manager/releases";

export interface VersionPolicy {
  /** The newest release, as `X.Y.Z`. */
  latest: string;
  /** Nodes below this version get the critical banner, as `X.Y.Z`. */
  minimum_supported: string;
}

export type UpgradeNotice =
  | { kind: "update"; current: string; latest: string }
  | { kind: "critical"; current: string; latest: string; minimum: string };

type Semver = [number, number, number];

const parseSemver = (version: string): Semver | null => {
  const m = /^v?(\d+)\.(\d+)\.(\d+)$/.exec(version.trim());
  return m ? [Number(m[1]), Number(m[2]), Number(m[3])] : null;
};

const compare = (a: Semver, b: Semver): number => a[0] - b[0] || a[1] - b[1] || a[2] - b[2];

const format = (v: Semver): string => v.join(".");

/**
 * Validates a fetched policy and normalizes both versions to `X.Y.Z`. Returns
 * null for anything malformed, so a broken file shows no banner rather than a
 * wrong one.
 */
export const parsePolicy = (raw: unknown): VersionPolicy | null => {
  if (typeof raw !== "object" || raw === null) return null;
  const { latest, minimum_supported } = raw as Record<string, unknown>;
  if (typeof latest !== "string" || typeof minimum_supported !== "string") return null;
  const l = parseSemver(latest);
  const m = parseSemver(minimum_supported);
  if (!l || !m || compare(m, l) > 0) return null;
  return { latest: format(l), minimum_supported: format(m) };
};

/** The banner a node on `current` should show, or null for none. */
export const upgradeNotice = (current: string, policy: VersionPolicy): UpgradeNotice | null => {
  const cur = parseSemver(current);
  const latest = parseSemver(policy.latest);
  const minimum = parseSemver(policy.minimum_supported);
  // A `dev` build has no comparable version.
  if (!cur || !latest || !minimum) return null;
  if (compare(cur, minimum) < 0) {
    return {
      kind: "critical",
      current: format(cur),
      latest: format(latest),
      minimum: format(minimum),
    };
  }
  if (compare(cur, latest) < 0) {
    return { kind: "update", current: format(cur), latest: format(latest) };
  }
  return null;
};

export const releaseNotesUrl = (version: string): string => `${RELEASES_URL}/tag/v${version}`;
