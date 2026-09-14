import { describe, expect, it } from "vitest";

import {
  canonicalPeerId,
  mergePeers,
  parsePeersCsv,
  peersToCsv,
} from "./peerCsv";
import type { Peer } from "./types";

/** A 34-byte namespace and a 33-byte compressed key, as the backend parses. */
const ns = (c: string): string => c.repeat(68);
const key = (c: string): string => `02${c.repeat(64)}`;

const ID_A = `participant1::${ns("a")}`;
const ID_B = `participant2::${ns("b")}`;

const peer = (over: Partial<Peer> = {}): Peer => ({
  participant_id: ID_A,
  name: "Node One",
  address: "node1.example.com",
  port: 9000,
  public_key: key("a"),
  ...over,
});

describe("peersToCsv", () => {
  it("writes a header and one row per peer", () => {
    const csv = peersToCsv([
      peer(),
      peer({ participant_id: ID_B, name: "Node Two", port: 9001 }),
    ]);

    expect(csv.split("\r\n")).toEqual([
      "participant_id,name,address,port,public_key,party",
      `${ID_A},Node One,node1.example.com,9000,${key("a")},`,
      `${ID_B},Node Two,node1.example.com,9001,${key("a")},`,
      "",
    ]);
  });

  it("quotes fields containing a comma, a quote, a newline or edge whitespace", () => {
    const csv = peersToCsv([
      peer({ name: 'Acme, Inc. "HQ"\nEU', address: " padded.example.com " }),
    ]);

    expect(csv).toContain('"Acme, Inc. ""HQ""\nEU"');
    expect(csv).toContain('" padded.example.com "');
  });

  it("emits the optional party column when set", () => {
    const party = `alice::${ns("c")}`;
    const csv = peersToCsv([peer({ party })]);

    expect(csv).toContain(`${key("a")},${party}`);
  });
});

describe("parsePeersCsv", () => {
  it("round-trips what peersToCsv produced, quoting included", () => {
    const peers = [
      peer({ name: 'Acme, Inc. "HQ"', party: `alice::${ns("c")}` }),
      peer({ participant_id: ID_B, name: "Node Two", port: 65535 }),
    ];

    const { rows, rejected } = parsePeersCsv(peersToCsv(peers));

    expect(rejected).toEqual([]);
    expect(rows.map((r) => r.peer)).toEqual(peers);
  });

  it("accepts a file with no header row", () => {
    const { rows, rejected } = parsePeersCsv(
      `${ID_B},Node Two,node2.example.com,9001,${key("b")},\n`,
    );

    expect(rejected).toEqual([]);
    expect(rows).toHaveLength(1);
    expect(rows[0]?.peer.participant_id).toBe(ID_B);
  });

  it("accepts a row without the trailing party column", () => {
    const { rows, rejected } = parsePeersCsv(
      `${ID_A},Node One,node1.example.com,9000,${key("a")}`,
    );

    expect(rejected).toEqual([]);
    expect(rows[0]?.peer.party).toBeUndefined();
  });

  it("skips blank lines and tolerates CRLF or LF endings", () => {
    const { rows, rejected } = parsePeersCsv(
      "participant_id,name,address,port,public_key,party\r\n" +
        "\r\n" +
        `${ID_A},One,a.example.com,9000,${key("a")},\r\n` +
        "\n" +
        `${ID_B},Two,b.example.com,9001,${key("b")},\n`,
    );

    expect(rejected).toEqual([]);
    expect(rows.map((r) => r.peer.name)).toEqual(["One", "Two"]);
  });

  it("falls back to the participant id when the name is blank", () => {
    const { rows } = parsePeersCsv(`${ID_A},,a.example.com,9000,${key("a")},`);

    expect(rows[0]?.peer.name).toBe(ID_A);
  });

  // One bad row must not cost the user the rest of the file, so every
  // rejection is reported with the line it came from and the good rows survive.
  it("rejects bad rows individually and keeps the good ones", () => {
    const { rows, rejected } = parsePeersCsv(
      [
        "participant_id,name,address,port,public_key,party",
        `${ID_A},One,a.example.com,9000,${key("a")},`,
        `,Nameless,a.example.com,9000,${key("a")},`,
        `participant3::${ns("c")},Three,,9000,${key("c")},`,
        `participant4::${ns("d")},Four,d.example.com,9000,,`,
        `participant5::${ns("e")},Five,e.example.com,not-a-port,${key("e")},`,
        `participant6::${ns("f")},Six,f.example.com,70000,${key("f")},`,
        `participant7::${ns("0")},Seven`,
        `${ID_B},Two,b.example.com,9001,${key("b")},`,
      ].join("\n"),
    );

    expect(rows.map((r) => r.peer.name)).toEqual(["One", "Two"]);
    expect(rejected.map((r) => r.line)).toEqual([3, 4, 5, 6, 7, 8]);
    expect(rejected[0]?.reason).toBe("Missing participant_id");
    expect(rejected[1]?.reason).toBe("Missing address");
    expect(rejected[2]?.reason).toBe("Missing public_key");
    expect(rejected[3]?.reason).toContain('Invalid port "not-a-port"');
    expect(rejected[4]?.reason).toContain('Invalid port "70000"');
    expect(rejected[5]?.reason).toContain("Expected 5 or 6 columns");
  });

  // The backend deserializes participant_id as CantonId and runs public_key
  // through secp256k1, so a row it would refuse has to be caught here — one
  // such row otherwise fails the POST and costs every valid row with it.
  it("rejects ids and keys the backend would refuse", () => {
    const { rows, rejected } = parsePeersCsv(
      [
        `not-a-canton-id,One,a.example.com,9000,${key("a")},`,
        `nons::abcdef,Two,b.example.com,9000,${key("b")},`,
        `two::sep::${ns("c")},Three,c.example.com,9000,${key("c")},`,
        `participant4::${ns("d")},Four,d.example.com,9000,not-hex,`,
        `participant5::${ns("e")},Five,e.example.com,9000,${"ab".repeat(20)},`,
        `${ID_A},Good,a.example.com,9000,${key("a")},`,
      ].join("\n"),
    );

    expect(rows.map((r) => r.peer.name)).toEqual(["Good"]);
    expect(rejected.map((r) => r.line)).toEqual([1, 2, 3, 4, 5]);
    for (const r of rejected.slice(0, 3)) {
      expect(r.reason).toContain("Invalid participant_id");
    }
    for (const r of rejected.slice(3)) {
      expect(r.reason).toContain("Invalid public_key");
    }
  });

  // A stray unquoted comma shifts every field after it. Binding the first six
  // and dropping the rest would save a peer with someone else's key.
  it("rejects a row with more columns than the format has", () => {
    const { rows, rejected } = parsePeersCsv(
      `${ID_A},Acme, Inc.,a.example.com,9000,${key("a")},`,
    );

    expect(rows).toEqual([]);
    expect(rejected[0]?.reason).toContain("found 7");
  });

  // The backend lower-cases the namespace when it stores a CantonId, so two
  // spellings are one peer. Keying on the raw string would import both and
  // collide on the peers primary key.
  it("canonicalises the namespace so one id has one spelling", () => {
    const upper = `participant1::${ns("A")}`;
    const { rows, rejected } = parsePeersCsv(
      [
        `${upper},Upper,a.example.com,9000,${key("a")},`,
        `${ID_A},Lower,z.example.com,9999,${key("b")},`,
      ].join("\n"),
    );

    expect(rows).toHaveLength(1);
    expect(rows[0]?.peer.participant_id).toBe(ID_A);
    expect(rejected[0]?.reason).toContain("Duplicate participant_id");
  });

  it("rejects malformed quoting rather than guessing at it", () => {
    const unterminated = parsePeersCsv(
      `${ID_A},"Never closed,a.example.com,9000,${key("a")},`,
    );
    expect(unterminated.rows).toEqual([]);
    expect(unterminated.rejected[0]?.reason).toBe("Unterminated quoted field");

    const trailing = parsePeersCsv(
      `${ID_A},"Acme"Corp,a.example.com,9000,${key("a")},`,
    );
    expect(trailing.rows).toEqual([]);
    expect(trailing.rejected[0]?.reason).toBe(
      "Unexpected text after a closing quote",
    );

    const bare = parsePeersCsv(
      `${ID_A},Acme"Node,a.example.com,9000,${key("a")},`,
    );
    expect(bare.rows).toEqual([]);
    expect(bare.rejected[0]?.reason).toBe(
      "Unexpected quote in an unquoted field",
    );
  });

  // Hex of the right length is not enough: a secp256k1 key carries an 02/03
  // prefix compressed or 04 uncompressed. The curve check itself is the
  // backend's, which now refuses the POST rather than storing a dead peer.
  it("rejects a key whose SEC1 prefix is wrong", () => {
    const rowFor = (k: string) =>
      `${ID_A},One,a.example.com,9000,${k},`;

    expect(parsePeersCsv(rowFor(`05${"a".repeat(64)}`)).rows).toEqual([]);
    expect(parsePeersCsv(rowFor(`02${"a".repeat(128)}`)).rows).toEqual([]);
    expect(parsePeersCsv(rowFor(`03${"a".repeat(64)}`)).rows).toHaveLength(1);
    expect(parsePeersCsv(rowFor(`04${"a".repeat(128)}`)).rows).toHaveLength(1);
  });

  // peersToCsv quotes a value whose whitespace matters, so trimming it back
  // off would silently rewrite that peer and show it as an Update.
  it("keeps whitespace that was quoted and trims whitespace that was not", () => {
    const padded = peer({ address: " padded.example.com " });
    const { rows } = parsePeersCsv(peersToCsv([padded]));
    expect(rows[0]?.peer.address).toBe(" padded.example.com ");

    const loose = parsePeersCsv(
      `${ID_A} , Node One , a.example.com , 9000 , ${key("a")} ,`,
    );
    expect(loose.rejected).toEqual([]);
    expect(loose.rows[0]?.peer).toMatchObject({
      participant_id: ID_A,
      name: "Node One",
      address: "a.example.com",
      port: 9000,
    });
  });

  it("rejects the second row that repeats a participant id", () => {
    const { rows, rejected } = parsePeersCsv(
      [
        `${ID_A},One,a.example.com,9000,${key("a")},`,
        `${ID_A},One again,z.example.com,9999,${key("b")},`,
      ].join("\n"),
    );

    expect(rows).toHaveLength(1);
    expect(rows[0]?.peer.address).toBe("a.example.com");
    expect(rejected[0]?.reason).toContain("Duplicate participant_id");
  });

  it("returns nothing for an empty file or a header-only file", () => {
    expect(parsePeersCsv("")).toEqual({ rows: [], rejected: [] });
    expect(
      parsePeersCsv("participant_id,name,address,port,public_key,party\r\n"),
    ).toEqual({ rows: [], rejected: [] });
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
  const existingB = peer({ participant_id: "b::1220", name: "B", port: 9001 });

  it("updates a peer in place and appends the new ones", () => {
    const updatedB = peer({ participant_id: "b::1220", name: "B", port: 9999 });
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
