import { useState } from "react";

import * as api from "../api";
import type { PartyView } from "../types";
import { Card, Copy, Fact, Notice } from "./ui";

/** The party's identity, and an explicit statement of what the hosts do and do
 *  not hold. The custody line is the whole point of the tenant API. */
export function PartyPanel({
  party,
  hostCount,
  onReset,
}: {
  party: PartyView;
  hostCount: number;
  onReset: () => void;
}) {
  const [seed, setSeed] = useState<string | null>(null);
  const [seedError, setSeedError] = useState<string | null>(null);

  const reveal = async () => {
    try {
      const { seed: value } = await api.getSecret();
      setSeed(value);
      setSeedError(null);
    } catch (e) {
      setSeedError(e instanceof Error ? e.message : String(e));
    }
  };

  return (
    <Card
      title="Party"
      eyebrow="one key · many hosts"
      aside={
        <button className="btn-secondary btn-sm" type="button" onClick={onReset}>
          Reset demo
        </button>
      }
    >
      <div className="facts">
        <Fact label="Party id" value={party.party_id} />
        <Fact label="Namespace fingerprint" value={party.fingerprint} />
        <Fact label="Public key (base64)" value={party.public_key} />

        <div className="fact">
          <span className="t-eyebrow">Private key (base64 seed)</span>
          {seed ? (
            <div className="fact-value">
              <code className="secret">{seed}</code>
              <Copy value={seed} />
              <button
                type="button"
                className="copy"
                onClick={() => setSeed(null)}
                title="Hide"
              >
                Hide
              </button>
            </div>
          ) : (
            <div className="fact-value">
              <code className="secret masked">{"•".repeat(44)}</code>
              <button type="button" className="copy" onClick={() => void reveal()}>
                Reveal
              </button>
            </div>
          )}
          <p className="field-hint">
            This is the party's actual signing key, held only by this wallet. It is served
            over loopback to this page and is never sent to a host — that is the point of
            showing it. A production wallet would keep it behind a device unlock.
          </p>
        </div>
      </div>

      {seedError ? <Notice kind="error">Could not read the key: {seedError}</Notice> : null}

      <div className="custody">
        <span className="notice-glyph" style={{ color: "var(--accent)" }}>
          ◆
        </span>
        <p>
          <strong>No host has ever seen the key above.</strong> Each of the {hostCount} hosts
          received only the public key and one signature per topology transaction. None of
          them can transact as this party, and none can reconstruct the key.
        </p>
      </div>
    </Card>
  );
}
