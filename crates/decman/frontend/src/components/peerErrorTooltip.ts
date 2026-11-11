import type { PeerPackageResult } from "../types";

/// Why this node has no package list for a peer, in the operator's terms.
///
/// The comparison reads the synchronizer's topology store and contacts no
/// peer, so "unreachable" no longer describes any of these. A failed topology
/// read is this node's problem, and an empty vetting set is the peer's, so the
/// tooltip has to separate them: a stale synchronizer-id cache here would
/// otherwise read as every peer being down.
export function peerErrorTooltip(peer: PeerPackageResult): string {
  switch (peer.error_kind) {
    case "topology_read_failed":
      return "This node could not read the synchronizer's topology store — check this node's connection to the synchronizer";
    case "no_vetted_packages":
      return "This peer has vetted no packages — it cannot run any Daml until it uploads and vets them";
    case undefined:
    case null:
      return "No package list for this peer";
  }
}
