import { useCallback, useEffect, useState } from "react";

import {
  VERSION_POLICY_URL,
  parsePolicy,
  upgradeNotice,
  type UpgradeNotice,
  type VersionPolicy,
} from "./versionPolicy";

const DISMISSED_KEY = "dismissed-update-version";
const REFRESH_MS = 60 * 60 * 1000;

function readDismissed(): string | null {
  try {
    return localStorage.getItem(DISMISSED_KEY);
  } catch {
    return null;
  }
}

/**
 * Reads the release policy on mount and then hourly, and returns the upgrade
 * banner this node should show. Dismissing an update hides it until a newer
 * release appears. A critical notice cannot be dismissed.
 */
export function useUpgradeNotice(current: string = __APP_VERSION__) {
  const [policy, setPolicy] = useState<VersionPolicy | null>(null);
  const [dismissed, setDismissed] = useState<string | null>(readDismissed);

  useEffect(() => {
    let cancelled = false;
    const load = () =>
      fetch(__MOCK__ ? "/version-policy" : VERSION_POLICY_URL)
        .then((res) => (res.ok ? res.json() : null))
        .then((raw) => {
          if (!cancelled) setPolicy(parsePolicy(raw));
        })
        // An offline browser or a blocked GitHub keeps the last policy it read.
        .catch(() => {});
    load();
    const interval = setInterval(load, REFRESH_MS);
    return () => {
      cancelled = true;
      clearInterval(interval);
    };
  }, []);

  const notice: UpgradeNotice | null = policy ? upgradeNotice(current, policy) : null;
  const latest = notice?.kind === "update" ? notice.latest : null;

  const dismiss = useCallback(() => {
    if (!latest) return;
    try {
      localStorage.setItem(DISMISSED_KEY, latest);
    } catch {
      // Without storage the dismissal lasts until the page reloads.
    }
    setDismissed(latest);
  }, [latest]);

  const hidden = latest !== null && dismissed === latest;
  return { notice: hidden ? null : notice, dismiss };
}
