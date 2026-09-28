export const TAB_HASHES = ["parties", "packages", "config", "notifications"] as const;

export function parseHash(hash: string): {
  tab: number;
  partySlug: string | null;
  /// `#packages/<party id>`: the packages tab compares that party's hosts.
  packagesPartyId: string | null;
} {
  const raw = hash.replace(/^#\/?/, "");
  const [section, ...rest] = raw.split("/");
  const slug = rest.join("/") || null;

  const tabIndex = TAB_HASHES.indexOf(
    section as (typeof TAB_HASHES)[number],
  );
  return {
    tab: tabIndex >= 0 ? tabIndex : 0,
    partySlug: tabIndex === 0 ? slug : null,
    packagesPartyId: tabIndex === 1 ? slug : null,
  };
}

export function buildHash(tab: number, partySlug?: string | null): string {
  const section = TAB_HASHES[tab] ?? "parties";
  return partySlug ? `#${section}/${partySlug}` : `#${section}`;
}
