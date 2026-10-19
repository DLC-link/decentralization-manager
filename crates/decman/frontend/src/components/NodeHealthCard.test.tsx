import { render, screen } from "@testing-library/react";
import { createTheme } from "@mui/material";
import { describe, expect, it } from "vitest";

import { SnackbarProvider } from "../contexts";
import { NodeHealthCard } from "./NodeHealthCard";
import type { NodeConfig, NodeHealthResponse } from "../types";

// The card reads only these fields of the config.
const config = {
  node: { participant_id: `participant1::1220${"a".repeat(64)}` },
  canton: {
    admin_api_host: "participant",
    admin_api_port: 5002,
    ledger_api_host: "participant",
    ledger_api_port: 5001,
    synchronizer: "global",
  },
  version: "1.0.0",
} as unknown as NodeConfig;

const { palette } = createTheme();

// jsdom normalizes a color when it is assigned, so run the expected value
// through the same path as the rendered one before comparing.
const normalized = (color: string) => {
  const probe = document.createElement("div");
  probe.style.backgroundColor = color;
  return probe.style.backgroundColor;
};

const renderCard = (
  health: NodeHealthResponse | null,
  selfLatency: { ms?: number; seq: number },
) =>
  render(
    <SnackbarProvider>
      <NodeHealthCard config={config} health={health} selfLatency={selfLatency} />
    </SnackbarProvider>,
  );

// The tooltip labels the dot's wrapper; the last child is the solid dot (a
// live dot also renders a halo before it).
const dotColor = (label: string) => {
  const dot = screen.getByLabelText(label).lastElementChild as HTMLElement;
  return window.getComputedStyle(dot).backgroundColor;
};

describe("NodeHealthCard browser link", () => {
  it("shows a connected dot when the browser ping answers", () => {
    renderCard(null, { ms: 89, seq: 1 });

    expect(dotColor("GET /healthz — round-trip from this browser to your node")).toBe(
      normalized(palette.success.main),
    );
    expect(screen.getByText("89 ms")).toBeTruthy();
  });

  it("shows an unreachable dot when the browser ping gets no reply", () => {
    renderCard(null, { seq: 1 });

    expect(dotColor("GET /healthz — no reply from your node")).toBe(
      normalized(palette.error.main),
    );
    expect(screen.getByText("no reply")).toBeTruthy();
  });
});

describe("NodeHealthCard participant links", () => {
  it("colors each dot by whether its probe reached the participant", () => {
    const health: NodeHealthResponse = {
      status: "Degraded",
      checked_at: new Date().toISOString(),
      admin_api: { reachable: true, latency_ms: 10 },
      ledger_api: { reachable: false, error: "connection refused" },
      synchronizers: [],
    };
    renderCard(health, { ms: 89, seq: 1 });

    expect(dotColor("participant:5002 · ParticipantStatus — round-trip of the last probe")).toBe(
      normalized(palette.success.main),
    );
    expect(dotColor("participant:5001 · GetLedgerApiVersion — connection refused")).toBe(
      normalized(palette.error.main),
    );
  });
});
