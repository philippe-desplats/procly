import { describe, expect, it } from "vitest";

import { formatMoment } from "./time";

const NOV_5_AT_14_32_05_UTC = Date.UTC(2026, 10, 5, 14, 32, 5);

describe("an absolute time", () => {
  it("is the time of day when it is today", () => {
    const now = Date.UTC(2026, 10, 5, 18, 0, 0);

    expect(formatMoment(NOV_5_AT_14_32_05_UTC, now, "UTC")).toBe("14:32:05");
  });

  it("carries its date when it is another day", () => {
    const now = Date.UTC(2026, 10, 6, 9, 0, 0);

    expect(formatMoment(NOV_5_AT_14_32_05_UTC, now, "UTC")).toBe("2026-11-05 14:32:05");
  });

  it("is read in the zone it is asked for, midnight included", () => {
    const midnight = Date.UTC(2026, 10, 5, 22, 0, 0);

    expect(formatMoment(midnight, midnight, "Europe/Paris")).toBe("23:00:00");
    expect(formatMoment(Date.UTC(2026, 10, 5, 23, 30, 0), midnight, "Europe/Paris")).toBe(
      "2026-11-06 00:30:00",
    );
  });
});
