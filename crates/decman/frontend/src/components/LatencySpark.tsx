import { useTheme } from "@mui/material";
import { LATENCY_HISTORY } from "../useLatencyHistory";

/** Latency bands. Semantic, and separate from the brand accent. */
export type LatencyTone = "ok" | "warn" | "bad";

interface LatencySparkProps {
  /** Oldest to newest. Fewer than `slots` samples occupy the left of the track. */
  samples: number[];
  tone: LatencyTone;
  /** Total slots the track is scaled to, so a short series is not stretched. */
  slots?: number;
  label: string;
}

const WIDTH = 92;
const HEIGHT = 22;
const PAD = 2;

/**
 * A latency sparkline: area fill, line, emphasized endpoint.
 *
 * The y-axis is scaled to the window's own min/max, so the shape shows change
 * rather than absolute level — the number beside it carries the level. That is
 * what makes a link that has sat at 4 ms and is now at 88 ms read as a problem
 * before it crosses any threshold.
 *
 * Below two samples there is no shape to draw, so nothing is rendered rather
 * than a misleading flat line.
 */
export const LatencySpark = ({
  samples,
  tone,
  slots = LATENCY_HISTORY,
  label,
}: LatencySparkProps) => {
  const theme = useTheme();
  const stroke = {
    ok: theme.palette.success.main,
    warn: theme.palette.warning.main,
    bad: theme.palette.error.main,
  }[tone];

  if (samples.length < 2) return null;

  const lo = Math.min(...samples);
  const hi = Math.max(...samples);
  // A perfectly flat window would divide by zero; draw it along the baseline.
  const span = hi - lo || 1;
  const x = (i: number) => PAD + (i * (WIDTH - PAD * 2)) / (slots - 1);
  const y = (v: number) => HEIGHT - PAD - ((v - lo) / span) * (HEIGHT - PAD * 2);

  const points = samples.map((v, i) => `${x(i).toFixed(2)},${y(v).toFixed(2)}`);
  const lastX = x(samples.length - 1);
  const lastY = y(samples[samples.length - 1]);

  return (
    <svg
      width={WIDTH}
      height={HEIGHT}
      viewBox={`0 0 ${WIDTH} ${HEIGHT}`}
      role="img"
      aria-label={`${label}: last ${samples.length} samples, ${lo} to ${hi} ms`}
      style={{ display: "block" }}
    >
      <path
        d={`M${x(0).toFixed(2)},${HEIGHT - PAD} L${points.join(" L")} L${lastX.toFixed(2)},${HEIGHT - PAD} Z`}
        fill={stroke}
        opacity={0.14}
      />
      <polyline
        points={points.join(" ")}
        fill="none"
        stroke={stroke}
        strokeWidth={1.25}
        strokeLinejoin="round"
        strokeLinecap="round"
      />
      <circle cx={lastX.toFixed(2)} cy={lastY.toFixed(2)} r={1.9} fill={stroke} />
    </svg>
  );
};
