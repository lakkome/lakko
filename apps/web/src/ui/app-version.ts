// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What Settings > About shows. The version is `VITE_APP_VERSION` when the
 * build sets it (a release pipeline would), else the package version Vite
 * injects at build time as `__LAKKO_PKG_VERSION__` (see `vite.config.ts`).
 * `null` when neither names a real version: the web package carries the
 * placeholder `0.0.0` until a release sets one, and Settings shows no version
 * row rather than "0.0.0" (walk 7, 2026-11-20). A plain `node` run (the
 * tests) has neither.
 */

/** The licence every file in the repository carries. */
export const appLicense = "AGPL-3.0-or-later";

/** A version string worth showing: not empty and not the `0.0.0` placeholder. */
export function shownVersion(candidate: string | undefined | null): string | null {
  if (typeof candidate !== "string") return null;
  const v = candidate.trim();
  if (v.length === 0 || /^0+(\.0+)*$/.test(v)) return null;
  return v;
}

export function appVersion(): string | null {
  const env = import.meta.env as Record<string, string | undefined>;
  const fromEnv = shownVersion(env["VITE_APP_VERSION"]);
  if (fromEnv !== null) return fromEnv;
  return typeof __LAKKO_PKG_VERSION__ === "string" ? shownVersion(__LAKKO_PKG_VERSION__) : null;
}
