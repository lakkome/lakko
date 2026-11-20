// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

import { describe, expect, it } from "vitest";

import { formatAccuracy, formatCoordinates, isPoorFix, osmUrl, toSharedLocation } from "./location-format";

describe("location format", () => {
  it("formats coordinates with hemispheres and five decimals", () => {
    expect(formatCoordinates(523_702_200, 48_951_700, "en")).toBe("52.37022° N, 4.89517° E");
    expect(formatCoordinates(-338_688_000, -1_512_093_000, "en")).toBe("33.86880° S, 151.20930° W");
  });
  it("uses the locale's decimal separator", () => {
    expect(formatCoordinates(523_702_200, 48_951_700, "de")).toContain("52,37022");
  });
  it("formats accuracy in metres and kilometres", () => {
    expect(formatAccuracy(12, "en")).toMatch(/12\s?m/);
    expect(formatAccuracy(1400, "en")).toMatch(/1\.4\s?km/);
  });
  it("converts a fix to fixed point and drops an empty name", () => {
    const loc = toSharedLocation({ latitude: 52.3702201, longitude: 4.8951704, accuracy: 0.2 }, 1000, "  ");
    expect(loc).toEqual({ latE7: 523_702_201, lonE7: 48_951_704, accuracyM: 1, fixAtMs: 1000 });
    expect(toSharedLocation({ latitude: 0, longitude: 0, accuracy: 5 }, 1, " Cafe ").name).toBe("Cafe");
    expect(toSharedLocation({ latitude: 95, longitude: -200, accuracy: 5 }, 1, "").latE7).toBe(900_000_000);
  });
  it("marks a stale or coarse fix", () => {
    expect(isPoorFix(1500, 0, 0)).toBe(true);
    expect(isPoorFix(10, 0, 3 * 60 * 1000)).toBe(true);
    expect(isPoorFix(10, 0, 1000)).toBe(false);
  });
  it("builds the OpenStreetMap link", () => {
    expect(osmUrl(523_702_200, 48_951_700)).toBe(
      "https://www.openstreetmap.org/?mlat=52.37022&mlon=4.89517#map=17/52.37022/4.89517",
    );
  });
});
