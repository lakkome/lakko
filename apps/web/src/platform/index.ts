// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * Which app this build runs as (ADR-0085).
 *
 * `VITE_LAKKO_PLATFORM=desktop` builds the web client for the Tauri desktop
 * host: no service worker, no Web Push, and every other difference between
 * the browser and the desktop app asked here, never in a screen.
 */

/** The platforms a build can target. */
export type Platform = "web" | "desktop";

/** Read the build flag; anything but `desktop` is the web. */
export function platformFromEnv(value: string | undefined): Platform {
  return value === "desktop" ? "desktop" : "web";
}

/** The platform of this build. */
export const PLATFORM: Platform = platformFromEnv(
  import.meta.env.VITE_LAKKO_PLATFORM as string | undefined,
);

/** Whether this build runs inside the desktop app. */
export function isDesktop(): boolean {
  return PLATFORM === "desktop";
}

/** The global the desktop host sets, before any page script runs, to the
 *  Lakko origin it forwards `/api/*` to (`apps/desktop/src-tauri/src/main.rs`). */
export const DESKTOP_PUBLIC_ORIGIN_GLOBAL = "__LAKKO_PUBLIC_ORIGIN__";

/**
 * The origin of a link a person shares (`/i/`, `/c/`, `/g/`, a feed rule)
 * and of a link this client recognises as one of its own.
 *
 * In the browser that is the page's own origin. In the desktop app the page
 * is served from the app's internal scheme (`lakkoapp://localhost`), which no
 * one else can open, so the host tells the page the public origin instead. A
 * desktop page without it throws rather than mint a link that reaches nobody.
 */
export function publicOriginFor(
  platform: Platform,
  hostValue: unknown,
  locationOrigin: string | undefined,
): string {
  if (platform === "desktop") {
    if (typeof hostValue === "string" && /^https:\/\/[^/?#]+$/.test(hostValue)) {
      return hostValue;
    }
    throw new Error("desktop: the host did not provide the public origin");
  }
  return locationOrigin ?? "https://lakko.me";
}

/** [`publicOriginFor`] for this build and page. */
export function publicOrigin(): string {
  return publicOriginFor(
    PLATFORM,
    (globalThis as Record<string, unknown>)[DESKTOP_PUBLIC_ORIGIN_GLOBAL],
    typeof location !== "undefined" ? location.origin : undefined,
  );
}
