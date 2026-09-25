import { API_BASE } from "./constants";
import type { DecentralizedParty, PackageInfo, PeerPackageResult } from "./types";

export interface ParticipantOption {
  id: string;
  name: string;
}

/// A participant's display label: its peer name, else its id's prefix.
export function participantLabel(option: ParticipantOption): string {
  return option.name || option.id.split("::")[0];
}

/// The comparison endpoint for a selection. An empty selection compares every
/// configured peer, which is what the page did before it had a selector.
export function compareUrl(selected: string[]): string {
  const base = `${API_BASE}/packages/compare-peers`;
  if (selected.length === 0) return base;
  return `${base}?participants=${encodeURIComponent(selected.join(","))}`;
}

/// The party's hosting participants, less this node: this node's own list is
/// the reference the others are compared with.
export function partyParticipants(
  party: DecentralizedParty,
  selfParticipantId?: string,
): string[] {
  return party.participants
    .map((p) => p.participant_uid)
    .filter((id) => id !== selfParticipantId);
}

/// A named group of packages, matched by name prefix.
export interface PackageGroup {
  key: string;
  label: string;
  prefixes: string[];
}

export const PACKAGE_GROUPS: PackageGroup[] = [
  { key: "splice", label: "Splice", prefixes: ["splice-"] },
  { key: "utility", label: "Utility", prefixes: ["utility-"] },
  { key: "registry", label: "Registry", prefixes: ["utility-registry"] },
];

/// The terms in a free-form filter. Operators paste lists of DAR names, so
/// commas, spaces and new lines all separate terms.
export function filterTerms(raw: string): string[] {
  return raw
    .toLowerCase()
    .split(/[\s,;]+/)
    .map((t) => t.replace(/\.dar$/, ""))
    .filter(Boolean);
}

/// Whether a package belongs to the filtered set: it matches any selected
/// group or any term. With no group and no term, every package does.
export function matchesPackageFilter(
  pkg: { name: string; package_id: string },
  groups: PackageGroup[],
  terms: string[],
): boolean {
  if (groups.length === 0 && terms.length === 0) return true;
  const name = pkg.name.toLowerCase();
  const id = pkg.package_id.toLowerCase();
  return (
    groups.some((g) => g.prefixes.some((p) => name.startsWith(p))) ||
    terms.some((t) => name.includes(t) || id.includes(t))
  );
}

/// How one participant's vetting compares with one package on this node.
///
/// - `match`: it has vetted this exact package.
/// - `other_version`: it has vetted the same name at another version.
/// - `missing`: it has vetted no package of this name that this node knows.
/// - `unknown`: this node has no name for the package, so there is nothing to
///   compare by beyond the id.
/// - `unreachable`: this node has no package list for the participant.
export type CellStatus =
  | "match"
  | "other_version"
  | "missing"
  | "unknown"
  | "unreachable";

export interface PeerIndex {
  peer: PeerPackageResult;
  ids: Set<string>;
  versionsByName: Map<string, string[]>;
  /// Vetted ids this node holds no package for. The topology store carries
  /// only ids, so one of these can be another version of a package here.
  unnamed: number;
}

export function indexPeer(peer: PeerPackageResult): PeerIndex {
  const ids = new Set<string>();
  const versionsByName = new Map<string, string[]>();
  let unnamed = 0;
  for (const p of peer.packages) {
    ids.add(p.package_id);
    if (!p.name) {
      unnamed++;
      continue;
    }
    const versions = versionsByName.get(p.name) ?? [];
    if (!versions.includes(p.version)) versions.push(p.version);
    versionsByName.set(p.name, versions);
  }
  return { peer, ids, versionsByName, unnamed };
}

export interface Cell {
  status: CellStatus;
  /// The versions the participant has vetted, for `other_version`.
  versions: string[];
}

export function compareCell(index: PeerIndex, pkg: PackageInfo): Cell {
  if (!index.peer.reachable) return { status: "unreachable", versions: [] };
  if (index.ids.has(pkg.package_id)) return { status: "match", versions: [] };
  if (!pkg.name) return { status: "unknown", versions: [] };
  const versions = index.versionsByName.get(pkg.name) ?? [];
  // Same name and version under another id: a rebuild of the same source.
  if (versions.includes(pkg.version)) return { status: "match", versions: [] };
  if (versions.length > 0) return { status: "other_version", versions };
  return { status: "missing", versions: [] };
}

export interface ComparisonSummary {
  packages: number;
  differing: number;
  missing: number;
  unavailable: number;
}

/// Totals over the shown rows. A participant with no package list counts once,
/// in `unavailable`, and never as a difference.
export function summarize(
  rows: PackageInfo[],
  peers: PeerIndex[],
): ComparisonSummary {
  let differing = 0;
  let missing = 0;
  for (const pkg of rows) {
    for (const index of peers) {
      const { status } = compareCell(index, pkg);
      if (status === "other_version") differing++;
      if (status === "missing") missing++;
    }
  }
  return {
    packages: rows.length,
    differing,
    missing,
    unavailable: peers.filter((p) => !p.peer.reachable).length,
  };
}

/// Whether a row holds a difference at any reachable participant.
export function rowDiffers(pkg: PackageInfo, peers: PeerIndex[]): boolean {
  return peers.some((index) => {
    const { status } = compareCell(index, pkg);
    return status === "other_version" || status === "missing";
  });
}
