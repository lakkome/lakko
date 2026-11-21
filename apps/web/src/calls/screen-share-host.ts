// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * What the desktop app's host adds to the own share's mirror rule
 * (`docs/ux/screen-share.md` section 3.1): how many screens there are, which
 * the webview's page cannot tell, and the window's and its screen's sizes in
 * device pixels. Outside the desktop app, and when the host does not answer,
 * `undefined`.
 */
import { isDesktop } from "../platform";
import { HOST_APP_PATH } from "../platform/host";
import type { PixelSize } from "./screen-share-mirror";

/** The host's answer (`GET /__host/app/surfaces`). */
export interface HostSurfaces {
  readonly screens: number | undefined;
  readonly window: PixelSize | undefined;
  readonly screen: PixelSize | undefined;
}

function size(v: unknown): PixelSize | undefined {
  if (typeof v !== "object" || v === null) return undefined;
  const o = v as Record<string, unknown>;
  const w = o.width;
  const h = o.height;
  return typeof w === "number" && typeof h === "number" && w > 0 && h > 0 ? { width: w, height: h } : undefined;
}

/** Read the host's answer; anything malformed reads as unknown. */
export function parseHostSurfaces(v: unknown): HostSurfaces {
  const o = (typeof v === "object" && v !== null ? v : {}) as Record<string, unknown>;
  const screens = typeof o.screens === "number" && o.screens > 0 ? o.screens : undefined;
  return { screens, window: size(o.window), screen: size(o.screen) };
}

export async function hostSurfaces(): Promise<HostSurfaces | undefined> {
  if (!isDesktop()) return undefined;
  try {
    const r = await fetch(`${HOST_APP_PATH}/surfaces`);
    if (!r.ok) return undefined;
    return parseHostSurfaces(await r.json());
  } catch {
    return undefined;
  }
}
