import type { PartyView } from "../types";
import { Card, Fact } from "./ui";

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
      </div>
      <div className="custody">
        <span className="notice-glyph" style={{ color: "var(--accent)" }}>
          ◆
        </span>
        <p>
          <strong>The private key never left this wallet.</strong> Each of the {hostCount} hosts
          received the public key above and one signature over a hash it computed itself. None of
          them can transact as this party, and none of them can reconstruct the key.
        </p>
      </div>
    </Card>
  );
}
