// SPDX-License-Identifier: AGPL-3.0-or-later
// SPDX-FileCopyrightText: 2026 Lakko Contributors

/**
 * The desktop host's app routes (ADR-0085 section 4, `docs/ux/desktop.md`
 * sections 2, 3 and 3.1).
 *
 * The page talks to the Tauri host with `fetch` on its own origin, under
 * {@link HOST_APP_PATH}; the host talks to the page with a `lakko:host` DOM
 * event on `window`. Every function here is a no-op (or a refusal) outside
 * the desktop app, so a browser build never sends a host request.
 */
import { isDesktop } from "./index";

/** Path prefix of the host's app routes. */
export const HOST_APP_PATH = "/__host/app";

/** The DOM event the host dispatches on `window`. */
export const HOST_EVENT = "lakko:host";

/** What the host can tell the page. */
export type HostEventKind = "quit-requested" | "open-settings";

/** The desktop-only settings the host keeps on this computer. */
export interface HostSettings {
  keepRunning: boolean;
  openAtLogin: boolean;
}

/** What the page tells the host. */
export interface HostAppState {
  callLive: boolean;
  unread: number;
}

/** Thrown when a host request fails, with the HTTP status (0: not desktop). */
export class HostRequestError extends Error {
  readonly status: number;
  constructor(status: number, message: string) {
    super(message);
    this.name = "HostRequestError";
    this.status = status;
  }
}

function parseSettings(v: unknown): HostSettings {
  const o = (typeof v === "object" && v !== null ? v : {}) as Record<string, unknown>;
  return {
    keepRunning: typeof o.keepRunning === "boolean" ? o.keepRunning : true,
    openAtLogin: typeof o.openAtLogin === "boolean" ? o.openAtLogin : false,
  };
}

function requireDesktop(): void {
  if (!isDesktop()) throw new HostRequestError(0, "not the desktop app");
}

/**
 * Tell the host whether a call is live and how many conversations are
 * unread (the Dock badge; 0 removes it). Does nothing outside the desktop
 * app; a failure is swallowed (the badge is cosmetic, the call flag is
 * re-sent on the next change).
 */
export async function postHostState(state: HostAppState): Promise<void> {
  if (!isDesktop()) return;
  try {
    await fetch(`${HOST_APP_PATH}/state`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({
        callLive: state.callLive,
        unread: Math.max(0, Math.floor(state.unread)),
      }),
    });
  } catch {
    // The host is the page's own process; a failed post is retried with the
    // next state change.
  }
}

/** Read the desktop-only settings. */
export async function getHostSettings(): Promise<HostSettings> {
  requireDesktop();
  const res = await fetch(`${HOST_APP_PATH}/settings`, { method: "GET" });
  if (!res.ok) throw new HostRequestError(res.status, "settings read failed");
  return parseSettings(await res.json());
}

/** Write part of the desktop-only settings; returns the settings now in force. */
export async function setHostSettings(patch: Partial<HostSettings>): Promise<HostSettings> {
  requireDesktop();
  const res = await fetch(`${HOST_APP_PATH}/settings`, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(patch),
  });
  if (!res.ok) throw new HostRequestError(res.status, "settings write failed");
  return parseSettings(await res.json());
}

/** Ask the host to quit. Only after the person confirmed. */
export async function requestHostQuit(): Promise<void> {
  requireDesktop();
  const res = await fetch(`${HOST_APP_PATH}/quit`, { method: "POST" });
  if (!res.ok) throw new HostRequestError(res.status, "quit failed");
}

/**
 * Subscribe to one kind of host event. Returns the unsubscribe function.
 * Outside the desktop app nothing is subscribed.
 */
export function onHostEvent(kind: HostEventKind, handler: () => void): () => void {
  if (!isDesktop()) return () => {};
  const listener = (e: Event): void => {
    const detail = (e as CustomEvent<unknown>).detail as { kind?: unknown } | null;
    if (detail !== null && typeof detail === "object" && detail.kind === kind) handler();
  };
  window.addEventListener(HOST_EVENT, listener);
  return () => window.removeEventListener(HOST_EVENT, listener);
}
