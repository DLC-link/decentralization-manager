import { useState } from "react";

import { Card } from "./ui";

/** Step one of the demo: name the party and choose how many hosts must confirm. */
export function CreateParty({
  hostCount,
  defaultThreshold,
  busy,
  onCreate,
}: {
  hostCount: number;
  defaultThreshold: number | null;
  busy: boolean;
  onCreate: (partyHint: string, threshold: number | null) => void;
}) {
  const [partyHint, setPartyHint] = useState("alice");
  // `null` means "let DecMan default it to N-1".
  const [threshold, setThreshold] = useState<number | null>(defaultThreshold);

  const thresholdOptions = Array.from({ length: hostCount - 1 }, (_, i) => i + 1);

  return (
    <Card
      title="Create a co-validated party"
      eyebrow={`${hostCount} hosts configured`}
    >
      <form
        onSubmit={(event) => {
          event.preventDefault();
          onCreate(partyHint.trim(), threshold);
        }}
      >
        <div className="form-row">
          <label className="field">
            <span>Party hint</span>
            <input
              value={partyHint}
              onChange={(e) => setPartyHint(e.target.value)}
              placeholder="alice"
              spellCheck={false}
              required
            />
          </label>
          <label className="field">
            <span>Confirmation threshold</span>
            <select
              value={threshold === null ? "default" : String(threshold)}
              onChange={(e) =>
                setThreshold(e.target.value === "default" ? null : Number(e.target.value))
              }
            >
              <option value="default">Default (N−1 = {hostCount - 1})</option>
              {thresholdOptions.map((n) => (
                <option value={n} key={n}>
                  {n} of {hostCount}
                </option>
              ))}
            </select>
          </label>
          <button className="btn-primary" type="submit" disabled={busy || !partyHint.trim()}>
            {busy ? "Onboarding…" : "Create party"}
          </button>
        </div>
        <p className="field-hint">
          The party id becomes <code className="mono">{partyHint.trim() || "hint"}::&lt;fingerprint&gt;</code>,
          where the fingerprint comes from a key this wallet is about to generate. A threshold of{" "}
          {hostCount} is rejected on purpose — it would leave no host able to exit later.
        </p>
      </form>
    </Card>
  );
}
