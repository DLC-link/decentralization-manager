import { describe, expect, it } from "vitest";

import {
  canonicalPeerId,
  mergePeers,
  parsePeersCsv,
  peersToCsv,
} from "./peerCsv";
import type { Peer } from "./types";

/** A 34-byte namespace, the length CantonId parses. */
const ns = (c: string): string => c.repeat(68);

const ID_A = `participant1::${ns("a")}`;
const ID_B = `participant2::${ns("b")}`;
const PARTY_A = `node1::${ns("c")}`;
const PARTY_B = `node2::${ns("d")}`;

const peer = (over: Partial<Peer> = {}): Peer => ({
  participant_id: ID_A,
  name: "Node One",
  party: PARTY_A,
  ...over,
});

describe("peersToCsv", () => {
  it("writes a header and one row per peer", () => {
    const csv = peersToCsv([
      peer(),
      peer({ participant_id: ID_B, party: PARTY_B, name: "Node Two" }),
    ]);

    expect(csv.split("\r\n")).toEqual([
      "participant_id,node_party_id,name",
      `${ID_A},${PARTY_A},Node One`,
      `${ID_B},${PARTY_B},Node Two`,
      "",
    ]);
  });

  it("quotes fields containing a comma, a quote, a newline or edge whitespace", () => {
    const csv = peersToCsv([peer({ name: 'Acme, Inc. "HQ"\nEU' })]);

    expect(csv).toContain('"Acme, Inc. ""HQ""\nEU"');
    expect(peersToCsv([peer({ name: " padded " })])).toContain('" padded "');
  });
});

describe("parsePeersCsv", () => {
  it("round-trips what peersToCsv produced, quoting included", () => {
    const peers = [
      peer({ name: 'Acme, Inc. "HQ"' }),
      peer({ participant_id: ID_B, party: PARTY_B, name: "Node Two" }),
    ];

    const { rows, rejected } = parsePeersCsv(peersToCsv(peers));

    expect(rejected).toEqual([]);
    expect(rows.map((r) => r.peer)).toEqual(peers);
  });

  it("accepts a file with no header row", () => {
    const { rows, rejected } = parsePeersCsv(
      `${ID_B},${PARTY_B},Node Two\n`,
    );

    expect(rejected).toEqual([]);
    expect(rows).toHaveLength(1);
    expect(rows[0]?.peer.participant_id).toBe(ID_B);
  });

  // One copied row is the identity string the "Share my identity" button
  // produces, so a paste of it has to import exactly as a file would.
  it("accepts a row without the trailing name column", () => {
    const { rows, rejected } = parsePeersCsv(`${ID_A},${PARTY_A}`);

    expect(rejected).toEqual([]);
    expect(rows[0]?.peer.party).toBe(PARTY_A);
    expect(rows[0]?.peer.name).toBe(ID_A);
  });

  it("skips blank lines and tolerates CRLF or LF endings", () => {
    const { rows, rejected } = parsePeersCsv(
      "participant_id,node_party_id,name\r\n" +
        "\r\n" +
        `${ID_A},${PARTY_A},One\r\n` +
        "\n" +
        `${ID_B},${PARTY_B},Two\n`,
    );

    expect(rejected).toEqual([]);
    expect(rows.map((r) => r.peer.name)).toEqual(["One", "Two"]);
  });

  it("falls back to the participant id when the name is blank", () => {
    const { rows } = parsePeersCsv(`${ID_A},${PARTY_A},`);

    expect(rows[0]?.peer.name).toBe(ID_A);
  });

  // One bad row must not cost the user the rest of the file, so every
  // rejection is reported with the line it came from and the good rows survive.
  it("rejects bad rows individually and keeps the good ones", () => {
    const { rows, rejected } = parsePeersCsv(
      [
        "participant_id,node_party_id,name",
        `${ID_A},${PARTY_A},One`,
        `,${PARTY_A},Nameless`,
        `participant3::${ns("e")},,Three`,
        `participant4::${ns("f")}`,
        `${ID_B},${PARTY_B},Two`,
      ].join("\n"),
    );

    expect(rows.map((r) => r.peer.name)).toEqual(["One", "Two"]);
    expect(rejected.map((r) => r.line)).toEqual([3, 4, 5]);
    expect(rejected[0]?.reason).toBe("Missing participant_id");
    expect(rejected[1]?.reason).toBe("Missing node_party_id");
    expect(rejected[2]?.reason).toContain("Expected 2 or 3 columns");
  });

  // The backend deserializes both ids as CantonId, so a row it would refuse has
  // to be caught here — one such row otherwise fails the POST and costs every
  // valid row with it.
  it("rejects ids the backend would refuse", () => {
    const { rows, rejected } = parsePeersCsv(
      [
        `not-a-canton-id,${PARTY_A},One`,
        `nons::abcdef,${PARTY_A},Two`,
        `two::sep::${ns("e")},${PARTY_A},Three`,
        `participant4::${ns("f")},not-a-canton-id,Four`,
        `participant5::${ns("0")},nons::abcdef,Five`,
        `${ID_A},${PARTY_A},Good`,
      ].join("\n"),
    );

    expect(rows.map((r) => r.peer.name)).toEqual(["Good"]);
    expect(rejected.map((r) => r.line)).toEqual([1, 2, 3, 4, 5]);
    for (const r of rejected.slice(0, 3)) {
      expect(r.reason).toContain("Invalid participant_id");
    }
    for (const r of rejected.slice(3)) {
      expect(r.reason).toContain("Invalid node_party_id");
    }
  });

  // A stray unquoted comma shifts every field after it. Binding the first three
  // and dropping the rest would save a peer under someone else's node party.
  it("rejects a row with more columns than the format has", () => {
    const { rows, rejected } = parsePeersCsv(`${ID_A},${PARTY_A},Acme, Inc.`);

    expect(rows).toEqual([]);
    expect(rejected[0]?.reason).toContain("found 4");
  });

  // The backend lower-cases the namespace when it stores a CantonId, so two
  // spellings are one peer. Keying on the raw string would import both and
  // collide on the peers primary key.
  it("canonicalises the namespace so one id has one spelling", () => {
    const upper = `participant1::${ns("A")}`;
    const { rows, rejected } = parsePeersCsv(
      [`${upper},${PARTY_A},Upper`, `${ID_A},${PARTY_B},Lower`].join("\n"),
    );

    expect(rows).toHaveLength(1);
    expect(rows[0]?.peer.participant_id).toBe(ID_A);
    expect(rejected[0]?.reason).toContain("Duplicate participant_id");
  });

  it("rejects malformed quoting rather than guessing at it", () => {
    const unterminated = parsePeersCsv(`${ID_A},${PARTY_A},"Never closed`);
    expect(unterminated.rows).toEqual([]);
    expect(unterminated.rejected[0]?.reason).toBe("Unterminated quoted field");

    const trailing = parsePeersCsv(`${ID_A},${PARTY_A},"Acme"Corp`);
    expect(trailing.rows).toEqual([]);
    expect(trailing.rejected[0]?.reason).toBe(
      "Unexpected text after a closing quote",
    );

    const bare = parsePeersCsv(`${ID_A},${PARTY_A},Acme"Node`);
    expect(bare.rows).toEqual([]);
    expect(bare.rejected[0]?.reason).toBe(
      "Unexpected quote in an unquoted field",
    );
  });

  // peersToCsv quotes a value whose whitespace matters, so trimming it back
  // off would silently rewrite that peer and show it as an Update.
  it("keeps whitespace that was quoted and trims whitespace that was not", () => {
    const padded = peer({ name: " Node One " });
    const { rows } = parsePeersCsv(peersToCsv([padded]));
    expect(rows[0]?.peer.name).toBe(" Node One ");

    const loose = parsePeersCsv(`${ID_A} , ${PARTY_A} , Node One `);
    expect(loose.rejected).toEqual([]);
    expect(loose.rows[0]?.peer).toMatchObject({
      participant_id: ID_A,
      party: PARTY_A,
      name: "Node One",
    });
  });

  it("rejects the second row that repeats a participant id", () => {
    const { rows, rejected } = parsePeersCsv(
      [`${ID_A},${PARTY_A},One`, `${ID_A},${PARTY_B},One again`].join("\n"),
    );

    expect(rows).toHaveLength(1);
    expect(rows[0]?.peer.name).toBe("One");
    expect(rejected[0]?.reason).toContain("Duplicate participant_id");
  });

  it("returns nothing for an empty file or a header-only file", () => {
    expect(parsePeersCsv("")).toEqual({ rows: [], rejected: [] });
    expect(parsePeersCsv("participant_id,node_party_id,name\r\n")).toEqual({
      rows: [],
      rejected: [],
    });
  });
});

describe("canonicalPeerId", () => {
  it("lower-cases the namespace and leaves everything else alone", () => {
    expect(canonicalPeerId(`Node::${ns("A")}`)).toBe(`Node::${ns("a")}`);
    expect(canonicalPeerId("not-an-id")).toBe("not-an-id");
  });
});

describe("mergePeers", () => {
  const existingA = peer({ participant_id: "a::1220", name: "A" });
  const existingB = peer({ participant_id: "b::1220", name: "B" });

  it("updates a peer in place and appends the new ones", () => {
    const updatedB = peer({
      participant_id: "b::1220",
      name: "B",
      party: PARTY_B,
    });
    const newC = peer({ participant_id: "c::1220", name: "C" });

    expect(mergePeers([existingA, existingB], [updatedB, newC])).toEqual([
      existingA,
      updatedB,
      newC,
    ]);
  });

  it("leaves peers absent from the import untouched", () => {
    expect(mergePeers([existingA, existingB], [])).toEqual([
      existingA,
      existingB,
    ]);
  });

  // The same peer spelled with an upper-case namespace must update the row it
  // already has, not append a second one the POST would reject.
  it("matches an existing peer whose id differs only in case", () => {
    const upper = peer({
      participant_id: `a::${"A".repeat(68)}`,
      name: "A renamed",
    });
    const existing = peer({ participant_id: `a::${"a".repeat(68)}`, name: "A" });

    const merged = mergePeers([existing], [upper]);

    expect(merged).toHaveLength(1);
    expect(merged[0]?.name).toBe("A renamed");
  });

  it("keeps the existing order rather than the import order", () => {
    const updatedA = peer({ participant_id: "a::1220", name: "A renamed" });

    expect(
      mergePeers([existingA, existingB], [existingB, updatedA]).map(
        (p) => p.participant_id,
      ),
    ).toEqual(["a::1220", "b::1220"]);
  });
});
