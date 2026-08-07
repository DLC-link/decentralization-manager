import { useCallback, useEffect, useState } from "react";

import logo from "./assets/bitsafe-logo.svg";
import * as api from "./api";
import type { AcsView, ConfigView, HostReport, PartyView, StatusView } from "./types";
import { Contracts } from "./components/Contracts";
import { CreateParty } from "./components/CreateParty";
import { HostList } from "./components/HostList";
import { OnboardingSteps, type Stage } from "./components/OnboardingSteps";
import { PartyPanel } from "./components/PartyPanel";
import { Card, Notice } from "./components/ui";

/** How often to re-ask every host where the party stands. Polling continues after
 *  the party is live on purpose: take a host down and its badge changes, which is
 *  the co-validation story made visible. */
const POLL_INTERVAL_MS = 3000;

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

export default function App() {
  const [config, setConfig] = useState<ConfigView | null>(null);
  const [party, setParty] = useState<PartyView | null>(null);
  const [status, setStatus] = useState<StatusView | null>(null);
  const [acs, setAcs] = useState<AcsView | null>(null);
  const [acsLoaded, setAcsLoaded] = useState(false);
  const [stage, setStage] = useState<Stage>("idle");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    api
      .getConfig()
      .then((loaded) => {
        setConfig(loaded);
        setParty(loaded.party);
        if (loaded.party) setStage("authorizing");
      })
      .catch((e: unknown) => setError(message(e)));
  }, []);

  useEffect(() => {
    if (!party) return;
    let cancelled = false;

    const poll = async () => {
      try {
        const latest = await api.getStatus();
        if (cancelled) return;
        setStatus(latest);
        if (latest.fully_hosted) setStage("live");
      } catch (e) {
        if (!cancelled) setError(message(e));
      }
    };

    void poll();
    const timer = window.setInterval(() => void poll(), POLL_INTERVAL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [party]);

  const refreshAcs = useCallback(async () => {
    setBusy(true);
    try {
      setAcs(await api.getAcs());
      setError(null);
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  }, []);

  const anyHosted = status?.hosts.some((h) => h.status === "hosted") ?? false;

  // One host having the party is enough to read its ledger — waiting for all of
  // them would hide the ACS whenever a host is down, which is precisely the case
  // co-validation is supposed to survive.
  useEffect(() => {
    if (anyHosted && !acsLoaded) {
      setAcsLoaded(true);
      void refreshAcs();
    }
  }, [anyHosted, acsLoaded, refreshAcs]);

  const onCreate = async (partyHint: string, threshold: number | null) => {
    setBusy(true);
    setError(null);
    setStage("signing");
    try {
      const onboarded = await api.createParty(partyHint, threshold);
      setStatus({
        party_id: onboarded.party_id,
        hosts: onboarded.hosts,
        fully_hosted: onboarded.hosts.every((h: HostReport) => h.status === "hosted"),
      });
      setParty(await api.getParty());
      setStage("authorizing");
    } catch (e) {
      setError(message(e));
      setStage("idle");
    } finally {
      setBusy(false);
    }
  };

  const onReset = async () => {
    setBusy(true);
    try {
      await api.reset();
      setParty(null);
      setStatus(null);
      setAcs(null);
      setAcsLoaded(false);
      setStage("idle");
      setError(null);
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy(false);
    }
  };

  if (!config) {
    return (
      <div className="shell">
        {error ? <Notice kind="error">{error}</Notice> : <p className="muted">Loading…</p>}
      </div>
    );
  }

  const hostsUp = status?.hosts.filter((h) => h.status === "hosted").length ?? 0;

  return (
    <div className="shell">
      <header className="topbar">
        <div className="brand">
          <img src={logo} alt="BitSafe" />
          <span className="brand-divider" />
          <span className="mono t-sm">DecMan demo wallet</span>
        </div>
        <span className="badge badge-absent">
          <span className={`dot dot-live`} style={{ color: "var(--accent)" }} />
          {party ? `${hostsUp}/${config.hosts.length} hosts` : `${config.hosts.length} hosts`}
        </span>
      </header>

      <div className="hero">
        <div className="t-eyebrow">Co-validation · wallet-held key</div>
        <h1 className="t-h2">One key. Every host.</h1>
        <p>
          This wallet generates a Canton party's signing key, has DecMan host that party on several
          participants at once, and then transacts as it. The owner keeps sole control; the party
          keeps running when a host does not.
        </p>
      </div>

      {error ? <Notice kind="error">{error}</Notice> : null}

      <Card
        title="Hosting set"
        eyebrow={
          config.confirmation_threshold === null
            ? "threshold: N−1 (DecMan default)"
            : `threshold: ${config.confirmation_threshold} of ${config.hosts.length}`
        }
      >
        <HostList hosts={config.hosts} reports={status?.hosts ?? null} />
      </Card>

      {party ? (
        <PartyPanel party={party} hostCount={config.hosts.length} onReset={() => void onReset()} />
      ) : (
        <CreateParty
          hostCount={config.hosts.length}
          defaultThreshold={config.confirmation_threshold}
          busy={busy}
          onCreate={(hint, threshold) => void onCreate(hint, threshold)}
        />
      )}

      <Card title="How onboarding runs" eyebrow="the protocol, step by step">
        <OnboardingSteps stage={stage} />
        {stage === "authorizing" ? (
          <div style={{ marginTop: 20 }}>
            <Notice kind="info">
              Waiting for every host to authorize the topology. Canton keeps it a proposal until the
              last signature lands — this can take a couple of minutes.
            </Notice>
          </div>
        ) : null}
      </Card>

      {party ? (
        <Contracts acs={acs} busy={busy} onRefresh={() => void refreshAcs()} />
      ) : null}

      <footer className="footer">
        <span>DecMan tenant API · /v0/tenant/*</span>
        <span>The private key never leaves this process</span>
      </footer>
    </div>
  );
}
