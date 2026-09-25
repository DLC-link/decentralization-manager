import { API_BASE } from "./constants";
import type { DecentralizedParty } from "./types";

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
