export const TAB_HASHES = ["parties", "packages", "config", "notifications"] as const;

/// Parties-tab slug prefix for the parties another participant also hosts.
/// A slug with a "/" cannot be a party id or a prefix, so the two never clash.
const SHARED_SLUG_PREFIX = "shared/";

export function parseHash(hash: string): {
  tab: number;
  partySlug: string | null;
  /// `#packages/<party id>`: the packages tab compares that party's hosts.
  packagesPartyId: string | null;
  /// `#parties/shared/<participant id>`: the parties that participant hosts.
  sharedWith: string | null;
  /// `#config/<participant id>`: the configuration tab shows that peer's row.
  configPeerId: string | null;
} {
  const raw = hash.replace(/^#\/?/, "");
  const [section, ...rest] = raw.split("/");
  const slug = rest.join("/") || null;

  const tabIndex = TAB_HASHES.indexOf(
    section as (typeof TAB_HASHES)[number],
  );
  const tab = tabIndex >= 0 ? tabIndex : 0;
  const isShared = tabIndex === 0 && !!slug?.startsWith(SHARED_SLUG_PREFIX);
  return {
    tab,
    partySlug: tabIndex === 0 && !isShared ? slug : null,
    packagesPartyId: tabIndex === 1 ? slug : null,
    sharedWith: isShared
      ? slug!.slice(SHARED_SLUG_PREFIX.length) || null
      : null,
    configPeerId: tabIndex === 2 ? slug : null,
  };
}

export function buildHash(tab: number, partySlug?: string | null): string {
  const section = TAB_HASHES[tab] ?? "parties";
  return partySlug ? `#${section}/${partySlug}` : `#${section}`;
}

/// The parties-tab slug that lists the parties `participantId` also hosts.
export function sharedWithSlug(participantId: string): string {
  return `${SHARED_SLUG_PREFIX}${participantId}`;
}
