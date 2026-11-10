/**
 * The highest threshold a kick may set. The namespace threshold counts the
 * owners left, and the signing threshold that follows it counts the hosts'
 * keys left. A host's kick takes one of each. An owner that hosts nothing
 * leaves every host in place. Mirrors `kick_threshold_bound` on the server.
 */
export const kickThresholdBound = (
  hosting: boolean,
  hostCount: number,
  ownerCount: number,
): number => {
  const hostsLeft = hostCount - (hosting ? 1 : 0);
  // A party with no cached owners bounds by its hosts alone.
  const ownersLeft = ownerCount > 0 ? ownerCount - 1 : hostsLeft;
  return Math.max(0, Math.min(hostsLeft, ownersLeft));
};
