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

/** Serialise peers to RFC 4180 CSV, header row included. */
export const peersToCsv = (peers: Peer[]): string => {
  const lines = [PEER_CSV_COLUMNS.join(",")];
  for (const peer of peers) {
    lines.push(
      [
        peer.participant_id,
        peer.name,
        peer.address,
        String(peer.port),
        peer.public_key,
        peer.party ?? "",
      ]
        .map(escapeField)
        .join(","),
    );
  }
  return `${lines.join("\r\n")}\r\n`;
};

/** Split CSV text into records of fields, honouring quotes and doubled quotes. */
const splitRecords = (text: string): { line: number; fields: string[] }[] => {
  const records: { line: number; fields: string[] }[] = [];
  let fields: string[] = [];
  let field = "";
  let quoted = false;
  let line = 1;
  let recordLine = 1;
  let started = false;

  const endField = () => {
    fields.push(field);
    field = "";
  };
  const endRecord = () => {
    endField();
    records.push({ line: recordLine, fields });
    fields = [];
    started = false;
  };

  for (let i = 0; i < text.length; i++) {
    const char = text[i];
    if (!started) {
      recordLine = line;
      started = true;
    }
    if (quoted) {
      if (char === '"') {
        if (text[i + 1] === '"') {
          field += '"';
          i++;
        } else {
          quoted = false;
        }
      } else {
        if (char === "\n") line++;
        field += char;
      }
      continue;
    }
    if (char === '"' && field === "") {
      quoted = true;
    } else if (char === ",") {
      endField();
    } else if (char === "\r") {
      // Swallowed; the \n that follows, or EOF, ends the record.
    } else if (char === "\n") {
      endRecord();
      line++;
    } else {
      field += char;
    }
  }
  if (started || field !== "" || fields.length > 0) endRecord();

  return records;
};

const isHeaderRow = (fields: string[]): boolean =>
  fields[0]?.trim().toLowerCase() === PEER_CSV_COLUMNS[0];

const isBlankRow = (fields: string[]): boolean =>
  fields.every((f) => f.trim() === "");

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

  for (const { line, fields } of splitRecords(text)) {
    if (isBlankRow(fields)) continue;
    const wasFirst = first;
    first = false;
    if (wasFirst && isHeaderRow(fields)) continue;

    const raw = fields.join(",");
    if (fields.length < 5) {
      rejected.push({
        line,
        raw,
        reason: `Expected at least 5 columns (${PEER_CSV_COLUMNS.slice(0, 5).join(", ")}), found ${fields.length}`,
      });
      continue;
    }

    const [participantId, name, address, portText, publicKey, party] = fields.map(
      (f) => f.trim(),
    );

    if (!participantId) {
      rejected.push({ line, raw, reason: "Missing participant_id" });
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
    const port = Number(portText);
    if (!Number.isInteger(port) || port < 1 || port > 65535) {
      rejected.push({
        line,
        raw,
        reason: `Invalid port "${portText}" (expected 1-65535)`,
      });
      continue;
    }
    if (seen.has(participantId)) {
      rejected.push({
        line,
        raw,
        reason: `Duplicate participant_id "${participantId}" in this file`,
      });
      continue;
    }
    seen.add(participantId);

    rows.push({
      line,
      peer: {
        participant_id: participantId,
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
  const byId = new Map(imported.map((p) => [p.participant_id, p]));
  const merged = existing.map((p) => byId.get(p.participant_id) ?? p);
  const existingIds = new Set(existing.map((p) => p.participant_id));
  for (const peer of imported) {
    if (!existingIds.has(peer.participant_id)) merged.push(peer);
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
