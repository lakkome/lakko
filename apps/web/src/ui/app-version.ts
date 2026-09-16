// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What Settings > About shows. The version is `VITE_APP_VERSION` when the
 * build sets it (a release pipeline would), else the package version Vite
 * injects at build time as `__LAKKO_PKG_VERSION__` (see `vite.config.ts`),
 * else "dev" (a plain `node` run, such as the tests).
 */

/** The licence every file in the repository carries. */
export const appLicense = "AGPL-3.0-or-later";

export function appVersion(): string {
  const env = import.meta.env as Record<string, string | undefined>;
  const fromEnv = env["VITE_APP_VERSION"];
  if (typeof fromEnv === "string" && fromEnv.length > 0) return fromEnv;
  if (typeof __LAKKO_PKG_VERSION__ === "string" && __LAKKO_PKG_VERSION__.length > 0) {
    return __LAKKO_PKG_VERSION__;
  }
  return "dev";
}
