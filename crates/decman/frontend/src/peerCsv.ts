import type { Peer } from "./types";

export const PEER_CSV_COLUMNS = [
  "participant_id",
  "name",
  "address",
  "port",
  "public_key",
  "party",
] as const;

export interface ParsedPeerRow {
  line: number;
  peer: Peer;
}

export interface RejectedPeerRow {
  line: number;
  reason: string;
  raw: string;
}

export interface ParsedPeersCsv {
  rows: ParsedPeerRow[];
  rejected: RejectedPeerRow[];
}

const needsQuoting = (field: string): boolean =>
  /[",\r\n]/.test(field) || field !== field.trim();

const escapeField = (value: string): string =>
  needsQuoting(value) ? `"${value.replace(/"/g, '""')}"` : value;

/** One peer as an RFC 4180 row, no trailing newline and no header. */
export const peerToCsvRow = (peer: Peer): string =>
  [
    peer.participant_id,
    peer.name,
    peer.address,
    String(peer.port),
    peer.public_key,
    peer.party ?? "",
  ]
    .map(escapeField)
    .join(",");

/** Serialise peers to RFC 4180 CSV, header row included. */
export const peersToCsv = (peers: Peer[]): string =>
  `${[PEER_CSV_COLUMNS.join(","), ...peers.map(peerToCsvRow)].join("\r\n")}\r\n`;

interface Field {
  value: string;
  /** Opened with a quote, so its whitespace is data and must not be trimmed. */
  quoted: boolean;
}

interface RawRecord {
  line: number;
  fields: Field[];
  /** Set when the record is not well-formed CSV; it is rejected, not parsed. */
  error?: string;
}

/** Split CSV text into records of fields, honouring quotes and doubled quotes. */
const splitRecords = (text: string): RawRecord[] => {
  const records: RawRecord[] = [];
  let fields: Field[] = [];
  let value = "";
  let quoted = false;
  let inQuotes = false;
  let afterQuote = false;
  let error: string | undefined;
  let line = 1;
  let recordLine = 1;
  let started = false;

  const endField = () => {
    fields.push({ value, quoted });
    value = "";
    quoted = false;
    afterQuote = false;
  };
  const endRecord = () => {
    endField();
    records.push({ line: recordLine, fields, error });
    fields = [];
    error = undefined;
    started = false;
  };

  for (let i = 0; i < text.length; i++) {
    const char = text[i];
    if (!started) {
      recordLine = line;
      started = true;
    }

    if (inQuotes) {
      if (char === '"') {
        if (text[i + 1] === '"') {
          value += '"';
          i++;
        } else {
          inQuotes = false;
          afterQuote = true;
        }
      } else {
        if (char === "\n") line++;
        value += char;
      }
      continue;
    }

    if (char === '"') {
      if (value === "" && !quoted && !afterQuote) {
        inQuotes = true;
        quoted = true;
      } else {
        error ??= afterQuote
          ? "Unexpected text after a closing quote"
          : "Unexpected quote in an unquoted field";
        value += char;
      }
    } else if (char === ",") {
      endField();
    } else if (char === "\r") {
      if (text[i + 1] !== "\n") {
        endRecord();
        line++;
      }
    } else if (char === "\n") {
      endRecord();
      line++;
    } else {
      if (afterQuote) {
        error ??= "Unexpected text after a closing quote";
      }
      value += char;
    }
  }
  if (inQuotes) error ??= "Unterminated quoted field";
  if (started || value !== "" || fields.length > 0) endRecord();

  return records;
};

const isHeaderRow = (fields: Field[]): boolean =>
  fields[0]?.value.trim().toLowerCase() === PEER_CSV_COLUMNS[0];

const isBlankRow = (fields: Field[]): boolean =>
  fields.every((f) => f.value.trim() === "");

// Mirrors CantonId::parse on the backend: exactly one "::" and a 34-byte
// (68 hex character) namespace. A row the backend would refuse must not reach
// the POST, or one bad row costs the whole import.
const isCantonId = (value: string): boolean => {
  const parts = value.split("::");
  return parts.length === 2 && /^[0-9a-fA-F]{68}$/.test(parts[1]);
};

// The shape secp256k1 keys have: hex, and either a 33-byte compressed key
// behind an 02/03 prefix or a 65-byte uncompressed one behind 04. Whether the
// point is actually on the curve is settled by `PublicKey::from_slice` on the
// backend, which now rejects the POST rather than storing an unusable peer.
const isNoisePublicKey = (value: string): boolean => {
  if (!/^[0-9a-fA-F]+$/.test(value)) return false;
  const prefix = value.slice(0, 2).toLowerCase();
  if (value.length === 66) return prefix === "02" || prefix === "03";
  return value.length === 130 && prefix === "04";
};

/**
 * The form the backend stores. `CantonId` parses the namespace as hex and
 * writes it back lower-case, so two spellings of one id collide on the peers
 * primary key; keying on the raw string would import both and fail the POST.
 */
export const canonicalPeerId = (id: string): string => {
  const parts = id.split("::");
  return parts.length === 2 ? `${parts[0]}::${parts[1].toLowerCase()}` : id;
};

/**
 * Parse peers out of CSV text, tolerating an optional header row, blank lines
 * and a missing trailing `party` column. A row that cannot become a peer is
 * returned in `rejected` rather than failing the whole file.
 */
export const parsePeersCsv = (text: string): ParsedPeersCsv => {
  const rows: ParsedPeerRow[] = [];
  const rejected: RejectedPeerRow[] = [];
  const seen = new Set<string>();
  let first = true;

  for (const { line, fields, error } of splitRecords(text)) {
    if (!error && isBlankRow(fields)) continue;
    const wasFirst = first;
    first = false;
    if (!error && wasFirst && isHeaderRow(fields)) continue;

    const raw = fields.map((f) => f.value).join(",");
    if (error) {
      rejected.push({ line, raw, reason: error });
      continue;
    }
    if (fields.length < 5 || fields.length > PEER_CSV_COLUMNS.length) {
      rejected.push({
        line,
        raw,
        reason: `Expected 5 or ${PEER_CSV_COLUMNS.length} columns (${PEER_CSV_COLUMNS.join(", ")}), found ${fields.length}`,
      });
      continue;
    }

    // Whitespace inside a quoted field is data — `peersToCsv` quotes exactly
    // those values, so trimming them would change a peer on a round-trip.
    const [participantId, name, address, portText, publicKey, party] =
      fields.map((f) => (f.quoted ? f.value : f.value.trim()));

    if (!participantId) {
      rejected.push({ line, raw, reason: "Missing participant_id" });
      continue;
    }
    if (!isCantonId(participantId)) {
      rejected.push({
        line,
        raw,
        reason: `Invalid participant_id "${participantId}" (expected prefix::<68 hex characters>)`,
      });
      continue;
    }
    if (!address) {
      rejected.push({ line, raw, reason: "Missing address" });
      continue;
    }
    if (!publicKey) {
      rejected.push({ line, raw, reason: "Missing public_key" });
      continue;
    }
    if (!isNoisePublicKey(publicKey)) {
      rejected.push({
        line,
        raw,
        reason: `Invalid public_key "${publicKey}" (expected 66 or 130 hex characters)`,
      });
      continue;
    }
    const port = Number(portText);
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      rejected.push({
        line,
        raw,
        reason: `Invalid port "${portText}" (expected 1-65535)`,
      });
      continue;
    }
    const canonicalId = canonicalPeerId(participantId);
    if (seen.has(canonicalId)) {
      rejected.push({
        line,
        raw,
        reason: `Duplicate participant_id "${participantId}" in this file`,
      });
      continue;
    }
    seen.add(canonicalId);

    rows.push({
      line,
      peer: {
        participant_id: canonicalId,
        name: name || participantId,
        address,
        port,
        ...(party ? { party } : {}),
        public_key: publicKey,
      },
    });
  }

  return { rows, rejected };
};

/**
 * Merge imported peers into the current list: a matching `participant_id` is
 * overwritten in place, the rest appended, peers absent from the import kept.
 */
export const mergePeers = (existing: Peer[], imported: Peer[]): Peer[] => {
  const byId = new Map(
    imported.map((p) => [canonicalPeerId(p.participant_id), p]),
  );
  const merged = existing.map(
    (p) => byId.get(canonicalPeerId(p.participant_id)) ?? p,
  );
  const existingIds = new Set(
    existing.map((p) => canonicalPeerId(p.participant_id)),
  );
  for (const peer of imported) {
    if (!existingIds.has(canonicalPeerId(peer.participant_id))) {
      merged.push(peer);
    }
  }
  return merged;
};

/** Trigger a browser download of `content` as `filename`. */
export const downloadCsv = (filename: string, content: string): void => {
  const blob = new Blob([content], { type: "text/csv;charset=utf-8" });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = filename;
  document.body.appendChild(link);
  link.click();
  document.body.removeChild(link);
  URL.revokeObjectURL(url);
};
