/**
 * A Canton identifier split into its parts: `name::fingerprint`, with an
 * optional `::suffix` such as the `::36-4` on a physical synchronizer id.
 */
export interface CantonIdParts {
  name: string;
  fingerprint: string;
  suffix: string;
}

const CANTON_ID = /^(.+)::([0-9a-f]{8,})(::[^:]+)?$/i;

export const splitCantonId = (id: string): CantonIdParts | null => {
  const m = CANTON_ID.exec(id);
  return m ? { name: m[1], fingerprint: m[2], suffix: m[3] ?? "" } : null;
};

/** The readable part of an id, or the whole id when it has no fingerprint. */
export const cantonIdName = (id: string): string => splitCantonId(id)?.name ?? id;

/** True when both ids carry the same namespace fingerprint. */
export const sameNamespace = (a: string, b: string): boolean => {
  const fa = splitCantonId(a)?.fingerprint;
  return !!fa && fa === splitCantonId(b)?.fingerprint;
};

/**
 * Every fingerprint opens with the same `1220` multihash prefix, so fewer
 * leading characters than this tell two namespaces apart by nothing.
 */
const MIN_FINGERPRINT_HEAD = 8;

/**
 * Shortens an id to `start` leading and `end` trailing characters.
 *
 * A Canton id keeps its whole name: the name is what a reader recognizes, so
 * only the fingerprint is cut. A long name therefore does not push the
 * fingerprint down to its constant `1220` prefix.
 */
export const shortenId = (text: string, start: number, end: number): string => {
  const parts = splitCantonId(text);
  if (!parts) {
    return text.length <= start + end ? text : `${text.slice(0, start)}...${text.slice(-end)}`;
  }
  const { name, fingerprint, suffix } = parts;
  const head = Math.max(MIN_FINGERPRINT_HEAD, start - name.length - 2);
  const shortFingerprint =
    fingerprint.length <= head + end
      ? fingerprint
      : `${fingerprint.slice(0, head)}...${fingerprint.slice(-end)}`;
  return `${name}::${shortFingerprint}${suffix}`;
};
