import type { DotTone } from "./components/StatusDot";
import type { ConnectionStatus, Peer } from "./types";

/**
 * Peer display and exchange helpers.
 *
 * Nodes no longer dial each other, so a peer has no address and no transport key.
 * A peer is the three values operators exchange out of band: the participant
 * id, the node party, and a display name (design D2).
 */

/** Shown where a peer has no node party. Such a peer cannot be invited. */
export const NO_NODE_PARTY = "(no node party)";

/**
 * Secondary line under a peer's name in the pickers. The node party is the
 * value that matters: it is the observer of every contract this node writes
 * for that peer.
 */
export const peerSubtitle = (peer: Peer): string =>
  peer.party || NO_NODE_PARTY;

/** The one string operators exchange: `participant_id,node_party_id,name`. */
export const peerIdentityString = (
  participantId: string,
  nodeParty: string,
  name: string,
): string => `${participantId},${nodeParty},${name}`;

/**
 * Parse a pasted identity string back into a peer. Returns `null` when the
 * participant id or the node party is missing, because a peer without both is
 * unusable. A name is optional and defaults to the participant id.
 */
export const parsePeerIdentity = (text: string): Peer | null => {
  const trimmed = text.trim();
  const first = trimmed.indexOf(",");
  const second = trimmed.indexOf(",", first + 1);
  if (first < 0) return null;
  const participantId = trimmed.slice(0, first).trim();
  // Only the first two fields are split off; a name may hold commas itself.
  const nodeParty = (second < 0 ? trimmed.slice(first + 1) : trimmed.slice(first + 1, second)).trim();
  const name = second < 0 ? "" : trimmed.slice(second + 1).trim();
  if (!participantId || !nodeParty) return null;
  return {
    participant_id: participantId,
    name: name || participantId,
    party: nodeParty,
  };
};

/**
 * Map a peer's registry status onto a dot tone. A peer is never `live`: nodes
 * no longer hold a connection, so a recent heartbeat is evidence of a past
 * moment, not of a channel that is open now (design D3).
 */
export const toneForPeer = (status?: ConnectionStatus): DotTone => {
  switch (status) {
    case "CurrentNode":
      return "self";
    case "Active":
      return "ok";
    case "Stale":
      return "warn";
    case "Unvetted":
      return "bad";
    default:
      return "idle";
  }
};

/**
 * Format a heartbeat age as a short "time ago" label. Nodes no longer probe
 * each other, so this is the age of a registry heartbeat, not a liveness
 * measurement (design D3).
 */
export const heartbeatAge = (secs?: number | null): string => {
  if (secs == null) return "—";
  // Clock skew between two nodes can date a heartbeat in the future.
  if (secs <= 0) return "now";
  if (secs < 60) return `${secs}s ago`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m ago`;
  if (secs < 86400) return `${Math.floor(secs / 3600)}h ago`;
  return `${Math.floor(secs / 86400)}d ago`;
};
