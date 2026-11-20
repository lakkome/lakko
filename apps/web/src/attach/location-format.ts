// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * `docs/ux/attachments.md` section 9 / ADR-0092 section 6 — a shared location
 * in fixed point (1e-7 degree), formatted for the person's locale. Nothing
 * here asks any server anything; `osmUrl` is opened only after a tap.
 */
import type { SharedLocation } from "./types";

/** A fix older than this reads as stale (UX 9.4, chosen). */
export const LOCATION_STALE_MS = 2 * 60 * 1000;
/** Accuracy worse than this reads as poor (UX 9.4, chosen). */
export const LOCATION_POOR_ACCURACY_M = 1000;
/** No fix after this long: the not-found state (UX 9.4, chosen). */
export const LOCATION_NO_FIX_MS = 30 * 1000;
/** Place name bound in characters (UX 9.2, chosen). */
export const LOCATION_NAME_MAX = 100;

const LAT_MAX_E7 = 900_000_000;
const LON_MAX_E7 = 1_800_000_000;

function degrees(e7: number, locale: string): string {
  return new Intl.NumberFormat(locale, { minimumFractionDigits: 5, maximumFractionDigits: 5 }).format(
    Math.abs(e7) / 1e7,
  );
}

/** `52.37022° N, 4.89517° E` in `locale`'s digits and separators. */
export function formatCoordinates(latE7: number, lonE7: number, locale: string): string {
  const ns = latE7 < 0 ? "S" : "N";
  const ew = lonE7 < 0 ? "W" : "E";
  return `${degrees(latE7, locale)}° ${ns}, ${degrees(lonE7, locale)}° ${ew}`;
}

/** `12 m` / `1.4 km`, by the locale's unit format. */
export function formatAccuracy(meters: number, locale: string): string {
  if (meters < 1000) {
    return new Intl.NumberFormat(locale, { style: "unit", unit: "meter", maximumFractionDigits: 0 }).format(
      Math.max(1, Math.round(meters)),
    );
  }
  return new Intl.NumberFormat(locale, { style: "unit", unit: "kilometer", maximumFractionDigits: 1 }).format(
    meters / 1000,
  );
}

/** The wire shape of a fix, clamped to the core's bounds. An empty name is
 *  absent. */
export function toSharedLocation(
  coords: { readonly latitude: number; readonly longitude: number; readonly accuracy: number },
  timestampMs: number,
  name: string,
): SharedLocation {
  const clamp = (v: number, max: number): number => Math.max(-max, Math.min(max, v));
  const trimmed = name.trim();
  return {
    latE7: clamp(Math.round(coords.latitude * 1e7), LAT_MAX_E7),
    lonE7: clamp(Math.round(coords.longitude * 1e7), LON_MAX_E7),
    accuracyM: Math.min(1_000_000, Math.max(1, Math.round(coords.accuracy))),
    fixAtMs: Math.round(timestampMs),
    ...(trimmed.length === 0 ? {} : { name: trimmed }),
  };
}

/** Whether a fix should be shown in the warning colour. */
export function isPoorFix(accuracyM: number, fixAtMs: number, nowMs: number): boolean {
  return accuracyM > LOCATION_POOR_ACCURACY_M || nowMs - fixAtMs > LOCATION_STALE_MS;
}

function plain(e7: number): string {
  return (e7 / 1e7).toFixed(7).replace(/0+$/, "").replace(/\.$/, "");
}

/** openstreetmap.org at the point; opened only after a tap (UX 9.3). */
export function osmUrl(latE7: number, lonE7: number): string {
  const lat = plain(latE7);
  const lon = plain(lonE7);
  return `https://www.openstreetmap.org/?mlat=${lat}&mlon=${lon}#map=17/${lat}/${lon}`;
}
