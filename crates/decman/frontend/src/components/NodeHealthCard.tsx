import { Box, Chip, Tooltip, Typography } from "@mui/material";
import { CopyableText } from "./CopyableText";
import { LatencySpark, type LatencyTone } from "./LatencySpark";
import { StatusDot } from "./StatusDot";
import { useLatencyHistory } from "../useLatencyHistory";
import type {
  ComponentHealth,
  LinkHealth,
  NodeConfig,
  NodeHealthResponse,
  NodeHealthStatus,
} from "../types";

interface NodeHealthCardProps {
  config: NodeConfig;
  /** `null` until the first `/node-health` poll lands. */
  health: NodeHealthResponse | null;
  /**
   * Round-trip from this browser to the node, with a sequence that advances on
   * every measurement so a repeated reading still records as a fresh sample.
   */
  selfLatency: { ms?: number; seq: number };
}

/**
 * Latency bands, per hop. A single threshold would be wrong on both: 40 ms
 * across the public internet is excellent, 40 ms to a participant in the same
 * cluster means something is wedged.
 */
const BROWSER_BANDS = { good: 200, warn: 800 };
const CANTON_BANDS = { good: 25, warn: 150 };

const toneFor = (
  ms: number | null | undefined,
  bands: { good: number; warn: number },
): LatencyTone => {
  if (ms == null) return "bad";
  if (ms <= bands.good) return "ok";
  if (ms <= bands.warn) return "warn";
  return "bad";
};

const VERDICT: Record<
  NodeHealthStatus,
  {
    label: string;
    color: "success" | "warning" | "error";
    dot: "Connected" | "HandshakeFailed" | "Unreachable";
  }
> = {
  Healthy: { label: "Healthy", color: "success", dot: "Connected" },
  Degraded: { label: "Degraded", color: "warning", dot: "HandshakeFailed" },
  Down: { label: "Down", color: "error", dot: "Unreachable" },
};

const COMPONENT_COLOR: Record<ComponentHealth["state"], "default" | "warning" | "error"> = {
  Ok: "default",
  Degraded: "warning",
  Failed: "error",
  Fatal: "error",
  Unknown: "warning",
};

const formatUptime = (seconds: number): string => {
  const d = Math.floor(seconds / 86400);
  const h = Math.floor((seconds % 86400) / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  return `${d}d ${String(h).padStart(2, "0")}h ${String(m).padStart(2, "0")}m`;
};

const formatAge = (checkedAt: string): string => {
  const seconds = Math.max(0, Math.round((Date.now() - Date.parse(checkedAt)) / 1000));
  if (!Number.isFinite(seconds)) return "";
  if (seconds < 60) return `updated ${seconds}s ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `updated ${minutes}m ago`;
  return `updated ${Math.floor(minutes / 60)}h ago`;
};

const LABEL_SX = { fontSize: "0.78rem", color: "text.secondary" } as const;
const VALUE_SX = {
  fontFamily: "var(--font-mono)",
  fontSize: "0.8rem",
  overflowWrap: "anywhere",
} as const;

const IdRow = ({ label, children }: { label: string; children: React.ReactNode }) => (
  <Box sx={{ display: "flex", flexDirection: "column", gap: 0.25, minWidth: 0 }}>
    <Typography sx={LABEL_SX}>{label}</Typography>
    {children}
  </Box>
);

interface LinkRowProps {
  label: string;
  probe: string;
  link: LinkHealth;
  /** History token: changes on every observation of this link. */
  token: string | number;
  bands: { good: number; warn: number };
  /** Self is always reachable if the page rendered at all. */
  dotStatus: "Connected" | "CurrentNode" | "Unreachable";
}

const LinkRow = ({ label, probe, link, token, bands, dotStatus }: LinkRowProps) => {
  const history = useLatencyHistory(link.reachable ? link.latency_ms : null, token);
  const tone = link.reachable ? toneFor(link.latency_ms, bands) : "bad";
  const reading = link.reachable && link.latency_ms != null ? `${link.latency_ms} ms` : "no reply";
  const tooltip = link.error
    ? `${probe} — ${link.error}`
    : `${probe} — round-trip of the last probe`;

  return (
    <Box
      sx={{
        display: "grid",
        gridTemplateColumns: {
          xs: "12px minmax(0, 1fr) auto",
          sm: "12px minmax(0, 1fr) auto 92px",
        },
        alignItems: "center",
        gap: 1.25,
        py: 0.9,
        borderTop: "1px solid",
        borderColor: "divider",
      }}
    >
      <StatusDot status={link.reachable ? dotStatus : "Unreachable"} title={tooltip} />
      <Box sx={{ minWidth: 0 }}>
        <Typography sx={{ fontSize: "0.85rem" }}>{label}</Typography>
        <Typography
          sx={{
            fontFamily: "var(--font-mono)",
            fontSize: "0.66rem",
            color: "text.disabled",
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
          }}
        >
          {probe}
        </Typography>
      </Box>
      <Typography
        sx={{
          fontFamily: "var(--font-mono)",
          fontSize: "0.85rem",
          fontVariantNumeric: "tabular-nums",
          textAlign: "right",
          whiteSpace: "nowrap",
          color:
            tone === "ok" ? "success.main" : tone === "warn" ? "warning.main" : "error.main",
        }}
      >
        {reading}
      </Typography>
      <Box sx={{ display: { xs: "none", sm: "block" } }}>
        <LatencySpark samples={history} tone={tone} label={label} />
      </Box>
    </Box>
  );
};

/**
 * The Config tab's Node block: what the node is, and whether it is working.
 *
 * Identity on the left, the measured hops on the right. A healthy node stays
 * quiet — component chips and the error band appear only when there is a fault
 * to show, so a calm card is information rather than the absence of it.
 */
export const NodeHealthCard = ({ config, health, selfLatency }: NodeHealthCardProps) => {
  const selfHistory = useLatencyHistory(selfLatency.ms, selfLatency.seq);
  const selfTone = toneFor(selfLatency.ms, BROWSER_BANDS);
  const verdict = health ? VERDICT[health.status] : null;
  const participant = health?.participant;
  // Ok components are the norm and say nothing; only faults earn a chip.
  const faults = participant?.components.filter((c) => c.state !== "Ok") ?? [];
  const synchronizers = health?.synchronizers ?? [];

  // An empty list means three different things, and saying "not connected" for
  // all of them would put a claim on screen the node never made: before the
  // first poll nothing has been asked, and when the Admin API returns no
  // status the participant was asked but did not answer.
  const unlinked = !health
    ? {
        label: config.canton.synchronizer,
        color: "text.disabled",
        tooltip: "Configured synchronizer. Waiting for the first health poll.",
      }
    : !participant
      ? {
          label: "unknown",
          color: "text.disabled",
          tooltip:
            "The Admin API returned no participant status, so the synchronizer connection could not be read.",
        }
      : {
          label: "not connected",
          color: "warning.main",
          tooltip:
            "The participant reports no synchronizer connection. Nothing confirms or settles while this is the case.",
        };

  return (
    <Box>
      <Box
        sx={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          flexWrap: "wrap",
          gap: 1.5,
          pb: 1.75,
          borderBottom: "1px solid",
          borderColor: "divider",
        }}
      >
        <Box sx={{ display: "flex", alignItems: "center", gap: 1.5, flexWrap: "wrap" }}>
          <Typography variant="subtitle2" color="text.secondary">
            Node
          </Typography>
          {verdict && (
            <Box
              sx={{
                display: "inline-flex",
                alignItems: "center",
                gap: 0.9,
                px: 1.25,
                py: 0.4,
                borderRadius: 999,
                border: "1px solid",
                borderColor: `${verdict.color}.main`,
                color: `${verdict.color}.main`,
                bgcolor: "action.hover",
                fontFamily: "var(--font-mono)",
                fontSize: "0.68rem",
                fontWeight: 500,
                letterSpacing: "0.1em",
                textTransform: "uppercase",
              }}
            >
              <StatusDot status={verdict.dot} />
              {verdict.label}
            </Box>
          )}
        </Box>
        {health && (
          <Typography
            sx={{
              fontFamily: "var(--font-mono)",
              fontSize: "0.68rem",
              color: "text.disabled",
            }}
          >
            {formatAge(health.checked_at)}
          </Typography>
        )}
      </Box>

      <Box
        sx={{
          display: "grid",
          gridTemplateColumns: { xs: "minmax(0, 1fr)", md: "minmax(0, 1fr) minmax(0, 1.05fr)" },
          gap: { xs: 2.75, md: 3.5 },
          pt: 2,
        }}
      >
        <Box sx={{ display: "flex", flexDirection: "column", gap: 1.5, minWidth: 0 }}>
          <Typography variant="subtitle2" color="text.secondary">
            Identity
          </Typography>
          <IdRow label="Participant ID">
            <CopyableText
              text={config.node.participant_id}
              truncate={{ start: 16, end: 8 }}
              variant="body2"
            />
          </IdRow>
          <IdRow label="Synchronizer">
            {synchronizers.length > 0 ? (
              <Box sx={{ display: "flex", flexDirection: "column", gap: 0.25 }}>
                {synchronizers.map((s) => (
                  <Typography
                    key={s.physical_synchronizer_id}
                    sx={{
                      ...VALUE_SX,
                      color: s.healthy ? "text.primary" : "warning.main",
                    }}
                  >
                    {s.physical_synchronizer_id}
                    {!s.healthy && " (unhealthy)"}
                  </Typography>
                ))}
              </Box>
            ) : (
              <Tooltip title={unlinked.tooltip} arrow>
                <Typography
                  tabIndex={0}
                  sx={{ ...VALUE_SX, color: unlinked.color, cursor: "help" }}
                >
                  {unlinked.label}
                </Typography>
              </Tooltip>
            )}
          </IdRow>
          <IdRow label="Uptime">
            <Typography sx={VALUE_SX}>
              {participant?.initialized ? formatUptime(participant.uptime_seconds) : "—"}
            </Typography>
          </IdRow>
          <IdRow label="Canton / DecMan">
            <Typography sx={VALUE_SX}>
              {participant?.version || "—"} · {config.build_version ?? config.version}
            </Typography>
          </IdRow>
        </Box>

        <Box sx={{ display: "flex", flexDirection: "column", gap: 1.5, minWidth: 0 }}>
          <Typography variant="subtitle2" color="text.secondary">
            Links
          </Typography>
          <Box>
            <Box
              sx={{
                display: "grid",
                gridTemplateColumns: {
                  xs: "12px minmax(0, 1fr) auto",
                  sm: "12px minmax(0, 1fr) auto 92px",
                },
                alignItems: "center",
                gap: 1.25,
                py: 0.9,
              }}
            >
              <StatusDot status="CurrentNode" title="Round-trip from this browser to your node" />
              <Box sx={{ minWidth: 0 }}>
                <Typography sx={{ fontSize: "0.85rem" }}>Browser → DecMan</Typography>
                <Typography
                  sx={{
                    fontFamily: "var(--font-mono)",
                    fontSize: "0.66rem",
                    color: "text.disabled",
                  }}
                >
                  GET /healthz
                </Typography>
              </Box>
              <Typography
                sx={{
                  fontFamily: "var(--font-mono)",
                  fontSize: "0.85rem",
                  fontVariantNumeric: "tabular-nums",
                  textAlign: "right",
                  whiteSpace: "nowrap",
                  color:
                    selfLatency.ms == null
                      ? "error.main"
                      : selfTone === "ok"
                        ? "success.main"
                        : selfTone === "warn"
                          ? "warning.main"
                          : "error.main",
                }}
              >
                {selfLatency.ms != null ? `${selfLatency.ms} ms` : "no reply"}
              </Typography>
              <Box sx={{ display: { xs: "none", sm: "block" } }}>
                <LatencySpark
                  samples={selfHistory}
                  tone={selfTone}
                  label="Browser to DecMan"
                />
              </Box>
            </Box>

            {health && (
              <>
                <LinkRow
                  label="DecMan → Admin API"
                  probe={`${config.canton.admin_api_host}:${config.canton.admin_api_port} · ParticipantStatus`}
                  link={health.admin_api}
                  token={health.checked_at}
                  bands={CANTON_BANDS}
                  dotStatus="Connected"
                />
                <LinkRow
                  label="DecMan → Ledger API"
                  probe={`${config.canton.ledger_api_host}:${config.canton.ledger_api_port} · GetLedgerApiVersion`}
                  link={health.ledger_api}
                  token={health.checked_at}
                  bands={CANTON_BANDS}
                  dotStatus="Connected"
                />
              </>
            )}
          </Box>

          {(faults.length > 0 ||
            participant?.initialized === false ||
            participant?.active === false) && (
            <Box
              sx={{
                display: "flex",
                flexWrap: "wrap",
                gap: 1,
                pt: 1.5,
                borderTop: "1px solid",
                borderColor: "divider",
              }}
            >
              {participant?.initialized === false && (
                <Chip
                  size="small"
                  color="warning"
                  label="participant · initializing"
                  sx={{ height: 20, fontFamily: "var(--font-mono)", fontSize: "0.68rem" }}
                />
              )}
              {participant?.active === false && (
                <Chip
                  size="small"
                  color="warning"
                  label="participant · passive replica"
                  sx={{ height: 20, fontFamily: "var(--font-mono)", fontSize: "0.68rem" }}
                />
              )}
              {faults.map((c) => (
                <Tooltip key={c.name} title={c.description ?? ""} arrow>
                  <Chip
                    size="small"
                    color={COMPONENT_COLOR[c.state]}
                    label={`${c.name} · ${c.state.toLowerCase()}`}
                    sx={{ height: 20, fontFamily: "var(--font-mono)", fontSize: "0.68rem" }}
                  />
                </Tooltip>
              ))}
            </Box>
          )}

          {health?.status === "Down" && (
            <Box
              sx={{
                mt: 0.5,
                px: 1.5,
                py: 1.25,
                borderLeft: "3px solid",
                borderColor: "error.main",
                borderRadius: "0 8px 8px 0",
                bgcolor: "action.hover",
                fontFamily: "var(--font-mono)",
                fontSize: "0.72rem",
                overflowWrap: "anywhere",
              }}
            >
              {[health.admin_api, health.ledger_api].find((l) => !l.reachable)?.error ??
                "the participant did not answer"}
            </Box>
          )}
        </Box>
      </Box>
    </Box>
  );
};
