import { describe, expect, it } from "vitest";

import { mergePeers, parsePeersCsv, peersToCsv } from "./peerCsv";
import type { Peer } from "./types";

const peer = (over: Partial<Peer> = {}): Peer => ({
  participant_id: "participant1::1220aaaa",
  name: "Node One",
  address: "node1.example.com",
  port: 9000,
  public_key: "abc123",
  ...over,
});

describe("peersToCsv", () => {
  it("writes a header and one row per peer", () => {
    const csv = peersToCsv([
      peer(),
      peer({ participant_id: "participant2::1220bbbb", name: "Node Two", port: 9001 }),
    ]);

    expect(csv.split("\r\n")).toEqual([
      "participant_id,name,address,port,public_key,party",
      "participant1::1220aaaa,Node One,node1.example.com,9000,abc123,",
      "participant2::1220bbbb,Node Two,node1.example.com,9001,abc123,",
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
    const csv = peersToCsv([peer({ party: "alice::1220cccc" })]);

    expect(csv).toContain("abc123,alice::1220cccc");
  });
});

describe("parsePeersCsv", () => {
  it("round-trips what peersToCsv produced, quoting included", () => {
    const peers = [
      peer({ name: 'Acme, Inc. "HQ"', party: "alice::1220cccc" }),
      peer({ participant_id: "participant2::1220bbbb", name: "Node Two", port: 65535 }),
    ];

    const { rows, rejected } = parsePeersCsv(peersToCsv(peers));

    expect(rejected).toEqual([]);
    expect(rows.map((r) => r.peer)).toEqual(peers);
  });

  it("accepts a file with no header row", () => {
    const { rows, rejected } = parsePeersCsv(
      "participant2::1220bbbb,Node Two,node2.example.com,9001,def456,\n",
    );

    expect(rejected).toEqual([]);
    expect(rows).toHaveLength(1);
    expect(rows[0]?.peer.participant_id).toBe("participant2::1220bbbb");
  });

  it("accepts a row without the trailing party column", () => {
    const { rows, rejected } = parsePeersCsv(
      "participant1::1220aaaa,Node One,node1.example.com,9000,abc123",
    );

    expect(rejected).toEqual([]);
    expect(rows[0]?.peer.party).toBeUndefined();
  });

  it("skips blank lines and tolerates CRLF or LF endings", () => {
    const { rows, rejected } = parsePeersCsv(
      "participant_id,name,address,port,public_key,party\r\n" +
        "\r\n" +
        "participant1::1220aaaa,One,a.example.com,9000,k1,\r\n" +
        "\n" +
        "participant2::1220bbbb,Two,b.example.com,9001,k2,\n",
    );

    expect(rejected).toEqual([]);
    expect(rows.map((r) => r.peer.name)).toEqual(["One", "Two"]);
  });

  it("falls back to the participant id when the name is blank", () => {
    const { rows } = parsePeersCsv(
      "participant1::1220aaaa,,a.example.com,9000,k1,",
    );

    expect(rows[0]?.peer.name).toBe("participant1::1220aaaa");
  });

  // One bad row must not cost the user the rest of the file, so every
  // rejection is reported with the line it came from and the good rows survive.
  it("rejects bad rows individually and keeps the good ones", () => {
    const { rows, rejected } = parsePeersCsv(
      [
        "participant_id,name,address,port,public_key,party",
        "participant1::1220aaaa,One,a.example.com,9000,k1,",
        ",Nameless,a.example.com,9000,k1,",
        "participant3::1220cccc,Three,,9000,k3,",
        "participant4::1220dddd,Four,d.example.com,9000,,",
        "participant5::1220eeee,Five,e.example.com,not-a-port,k5,",
        "participant6::1220ffff,Six,f.example.com,70000,k6,",
        "participant7::12200000,Seven",
        "participant2::1220bbbb,Two,b.example.com,9001,k2,",
      ].join("\n"),
    );

    expect(rows.map((r) => r.peer.name)).toEqual(["One", "Two"]);
    expect(rejected.map((r) => r.line)).toEqual([3, 4, 5, 6, 7, 8]);
    expect(rejected[0]?.reason).toBe("Missing participant_id");
    expect(rejected[1]?.reason).toBe("Missing address");
    expect(rejected[2]?.reason).toBe("Missing public_key");
    expect(rejected[3]?.reason).toContain('Invalid port "not-a-port"');
    expect(rejected[4]?.reason).toContain('Invalid port "70000"');
    expect(rejected[5]?.reason).toContain("at least 5 columns");
  });

  it("rejects the second row that repeats a participant id", () => {
    const { rows, rejected } = parsePeersCsv(
      [
        "participant1::1220aaaa,One,a.example.com,9000,k1,",
        "participant1::1220aaaa,One again,z.example.com,9999,k9,",
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

  it("keeps the existing order rather than the import order", () => {
    const updatedA = peer({ participant_id: "a::1220", name: "A renamed" });

    expect(
      mergePeers([existingA, existingB], [existingB, updatedA]).map(
        (p) => p.participant_id,
      ),
    ).toEqual(["a::1220", "b::1220"]);
  });
});
