import { participantLabel } from "./packageCompare";
import type { DecentralizedParty, Peer } from "./types";

/// The parties that `participantId` hosts. The list holds only parties this
/// node hosts, so these are the parties the two participants share.
export function partiesSharedWith(
  parties: DecentralizedParty[],
  participantId: string,
): DecentralizedParty[] {
  return parties.filter((party) =>
    party.participants.some((p) => p.participant_uid === participantId),
  );
}

/// How many of `parties` each participant hosts, keyed by participant id.
export function sharedPartyCounts(
  parties: DecentralizedParty[],
): Map<string, number> {
  const counts = new Map<string, number>();
  for (const party of parties) {
    for (const { participant_uid } of party.participants) {
      counts.set(participant_uid, (counts.get(participant_uid) ?? 0) + 1);
    }
  }
  return counts;
}

/// A participant's display name: its peer name, else its id's prefix.
export function participantName(
  participantId: string,
  peers: Peer[] | null | undefined,
): string {
  const peer = peers?.find((p) => p.participant_id === participantId);
  return participantLabel({ id: participantId, name: peer?.name ?? "" });
}
