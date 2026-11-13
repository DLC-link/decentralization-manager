import { render, screen } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import { LatencySpark } from "./LatencySpark";

const spark = (samples: number[]) =>
  render(<LatencySpark samples={samples} tone="ok" label="Browser to DecMan" />);

describe("LatencySpark", () => {
  it("draws nothing until five samples have arrived", () => {
    spark([147, 150, 149, 151]);
    expect(screen.queryByRole("img")).toBeNull();
  });

  it("draws the line from the fifth sample on", () => {
    spark([147, 150, 149, 151, 148]);
    expect(screen.getByRole("img").getAttribute("aria-label")).toBe(
      "Browser to DecMan: last 5 samples, 147 to 151 ms",
    );
  });
});
